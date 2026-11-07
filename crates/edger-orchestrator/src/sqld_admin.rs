//! (EDG-54 / 14a) Cliente administrativo SQLD limitado, de uso exclusivo do host.
//!
//! Não é a API admin do EdgeR nem autoriza worker algum: é o cliente HTTP que o
//! futuro provisionador usa contra a API administrativa do operador SQLD.
//! Nenhum grant ou dado é criado aqui; este módulo não marca `Proven`/`Ready`,
//! não adota nem reconfigura namespaces e não faz dump/backup/dados/vault/
//! assinatura.
//!
//! Limites do que é prometido:
//! - Um slot por cliente (compartilhado entre clones) serializa as chamadas. A
//!   espera pelo slot, a conexão, os headers e o corpo dividem UM prazo total
//!   absoluto por chamada pública (`total_timeout`, 15 s por padrão). O prazo
//!   nasce antes da admissão; `probe_admin_auth` (dois GETs) usa o MESMO prazo
//!   nos dois. Estourá-lo devolve [`SqldAdminError::Timeout`]. Toda duração da
//!   configuração é positiva e no máximo [`MAX_CONFIG_TIMEOUT`] (60 s); o
//!   `read`/`connect` por etapa nunca estendem o prazo total.
//! - Sem proxy de ambiente e sem seguir redirects: um 3xx vira
//!   [`SqldAdminError::Redirect`] sem requisição ao `Location`.
//! - O corpo da resposta é lido só em 2xx, com teto de 64 KiB conferido por
//!   `Content-Length` e, antes de cada append, no loop de chunks.
//! - Nenhum retry automático: o cliente `reqwest` é construído com
//!   `reqwest::retry::never()` (o padrão do `reqwest` 0.12 repete NACKs de
//!   protocolo) e este módulo não tem laço próprio. Um `CREATE` com status inesperado ou timeout NÃO
//!   diz se o namespace foi criado; a recuperação (GET da chave pública) é do
//!   serviço chamador, sem POST de config nem DELETE.
//! - Erros e `Debug` são textos estáticos: nunca URL, nome, corpo, JWT ou chave.
//! - A chave crua vive num `Zeroizing<String>`, mas o `HeaderValue` montado por
//!   requisição e os buffers do `reqwest`/`hyper` não são zerados.
//! - A igualdade de chave pública pertence ao chamador (bytes públicos).
//!   `max_db_size` na resposta de config é lossy e não prova a quota.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, Method, StatusCode, Url};
use spki::der::DecodePem;
use spki::{ObjectIdentifier, SubjectPublicKeyInfoOwned};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use zeroize::Zeroizing;

use crate::sqld_registry::{
    SqldRegistryRecord, SqldRegistryState, MIN_QUOTA_BYTES, QUOTA_ALIGN_BYTES,
};
use crate::sqld_signer::PUBLIC_KEY_LEN;

/// Teto do corpo de resposta.
pub const MAX_ADMIN_RESPONSE_BYTES: usize = 64 * 1024;
/// Teto do campo de chave remota antes de decodificar.
pub const MAX_REMOTE_KEY_FIELD_BYTES: usize = 8 * 1024;
/// Prazo total padrão por chamada.
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(15);
/// Maior duração aceita em [`SqldAdminConfig`] (cada campo).
pub const MAX_CONFIG_TIMEOUT: Duration = Duration::from_secs(60);
/// Namespace fixo de probe (nunca criado por este cliente).
const PROBE_NAMESPACE: &str = "edger_probe";
const NAMESPACE_PREFIX: &str = "edger_";
const NAMESPACE_HEX_LEN: usize = 64;
/// RFC 8410 §3: id-Ed25519.
const ED25519_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.101.112");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldAdminError {
    InvalidOrigin,
    InvalidKey,
    InvalidConfig,
    InvalidNamespace,
    InvalidRecord,
    Client,
    Timeout,
    Transport,
    Redirect,
    Unauthorized,
    /// A API aceitou/respondeu sem credencial: falha fechado.
    ApiOpen,
    UnexpectedStatus,
    ResponseTooLarge,
    InvalidResponse,
}

impl fmt::Display for SqldAdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidOrigin => "sqld admin: invalid origin",
            Self::InvalidKey => "sqld admin: invalid admin key",
            Self::InvalidConfig => "sqld admin: invalid client config",
            Self::InvalidNamespace => "sqld admin: invalid namespace",
            Self::InvalidRecord => "sqld admin: invalid registry record",
            Self::Client => "sqld admin: cannot build http client",
            Self::Timeout => "sqld admin: deadline exceeded",
            Self::Transport => "sqld admin: transport error",
            Self::Redirect => "sqld admin: unexpected redirect",
            Self::Unauthorized => "sqld admin: unauthorized",
            Self::ApiOpen => "sqld admin: api accepts unauthenticated requests",
            Self::UnexpectedStatus => "sqld admin: unexpected status",
            Self::ResponseTooLarge => "sqld admin: response too large",
            Self::InvalidResponse => "sqld admin: invalid response",
        })
    }
}

impl std::error::Error for SqldAdminError {}

/// Prazos do cliente. Todos positivos e até [`MAX_CONFIG_TIMEOUT`]; `total`
/// governa a chamada inteira e sempre prevalece sobre `connect`/`read`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqldAdminConfig {
    pub total_timeout: Duration,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
}

impl Default for SqldAdminConfig {
    fn default() -> Self {
        Self {
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(10),
        }
    }
}

/// Resultado do `CREATE`: 400 NÃO prova que o namespace já existe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldCreateOutcome {
    Created,
    RejectedOrExists,
}

/// Estado da chave pública remota (`jwt_key`) de um namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldRemoteKey {
    Single([u8; PUBLIC_KEY_LEN]),
    Missing,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldNamespaceConfig {
    Missing,
    Present { key: SqldRemoteKey },
}

struct Inner {
    client: Client,
    origin: Url,
    key: Zeroizing<String>,
    slot: Semaphore,
    total_timeout: Duration,
}

/// Cliente administrativo; clones compartilham o mesmo slot.
#[derive(Clone)]
pub struct SqldAdminClient {
    inner: Arc<Inner>,
}

impl fmt::Debug for SqldAdminClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldAdminClient").finish_non_exhaustive()
    }
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl SqldAdminClient {
    pub fn new(origin: &str, key: Zeroizing<String>) -> Result<Self, SqldAdminError> {
        Self::with_config(origin, key, SqldAdminConfig::default())
    }

    pub fn with_config(
        origin: &str,
        key: Zeroizing<String>,
        config: SqldAdminConfig,
    ) -> Result<Self, SqldAdminError> {
        if [
            config.total_timeout,
            config.connect_timeout,
            config.read_timeout,
        ]
        .iter()
        .any(|d| d.is_zero() || *d > MAX_CONFIG_TIMEOUT)
        {
            return Err(SqldAdminError::InvalidConfig);
        }
        let origin = parse_origin(origin)?;
        // O servidor compara só o 2º token do header (`split_whitespace`);
        // só ASCII visível, sem espaço.
        if key.is_empty() || !key.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(SqldAdminError::InvalidKey);
        }
        let client = Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .build()
            .map_err(|_| SqldAdminError::Client)?;
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                origin,
                key,
                slot: Semaphore::new(1),
                total_timeout: config.total_timeout,
            }),
        })
    }

    /// GET de configuração do namespace fixo de probe: sem chave deve dar 401
    /// (200/404 = API aberta, falha fechado); com `Basic <chave crua>` deve
    /// autenticar (200 ou 404). Não cria dados.
    pub async fn probe_admin_auth(&self) -> Result<(), SqldAdminError> {
        let path = config_path(PROBE_NAMESPACE);
        // Um orçamento para os dois GETs, criado antes do primeiro.
        let deadline = self.new_deadline();
        let anon = self
            .execute(deadline, Method::GET, &path, false, None)
            .await?;
        match anon.status {
            StatusCode::UNAUTHORIZED => {}
            StatusCode::OK | StatusCode::NOT_FOUND => return Err(SqldAdminError::ApiOpen),
            other => return Err(status_error(other)),
        }
        let authed = self
            .execute(deadline, Method::GET, &path, true, None)
            .await?;
        match authed.status {
            StatusCode::OK | StatusCode::NOT_FOUND => Ok(()),
            other => Err(status_error(other)),
        }
    }

    /// `POST /v1/namespaces/{ns}/create` com EXATAMENTE `jwt_key`,
    /// `max_db_size` e `allow_attach: false`.
    pub async fn create_namespace(
        &self,
        record: &SqldRegistryRecord,
    ) -> Result<SqldCreateOutcome, SqldAdminError> {
        validate_namespace(&record.sql_namespace)?;
        if record.state == SqldRegistryState::Conflict
            || record.quota_bytes < MIN_QUOTA_BYTES
            || !record.quota_bytes.is_multiple_of(QUOTA_ALIGN_BYTES)
        {
            return Err(SqldAdminError::InvalidRecord);
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "jwt_key": URL_SAFE_NO_PAD.encode(record.public_key),
            "max_db_size": record.quota_bytes,
            "allow_attach": false,
        }))
        .map_err(|_| SqldAdminError::InvalidRecord)?;
        let path = format!("/v1/namespaces/{}/create", record.sql_namespace);
        let reply = self
            .execute(self.new_deadline(), Method::POST, &path, true, Some(body))
            .await?;
        match reply.status {
            StatusCode::OK => Ok(SqldCreateOutcome::Created),
            StatusCode::BAD_REQUEST => Ok(SqldCreateOutcome::RejectedOrExists),
            other => Err(status_error(other)),
        }
    }

    /// `GET /v1/namespaces/{ns}/config`: 404 = ausente; 200 = estado da chave.
    pub async fn read_config(
        &self,
        namespace: &str,
    ) -> Result<SqldNamespaceConfig, SqldAdminError> {
        validate_namespace(namespace)?;
        let reply = self
            .execute(
                self.new_deadline(),
                Method::GET,
                &config_path(namespace),
                true,
                None,
            )
            .await?;
        match reply.status {
            StatusCode::NOT_FOUND => Ok(SqldNamespaceConfig::Missing),
            StatusCode::OK => {
                // Só um objeto JSON vale (um array `["x"]` não passa por struct).
                let value: serde_json::Value = serde_json::from_slice(&reply.body)
                    .map_err(|_| SqldAdminError::InvalidResponse)?;
                let object = value.as_object().ok_or(SqldAdminError::InvalidResponse)?;
                let key = match object.get("jwt_key") {
                    None | Some(serde_json::Value::Null) => SqldRemoteKey::Missing,
                    Some(serde_json::Value::String(field)) => parse_remote_public_key(field),
                    Some(_) => return Err(SqldAdminError::InvalidResponse),
                };
                Ok(SqldNamespaceConfig::Present { key })
            }
            other => Err(status_error(other)),
        }
    }

    /// Prazo absoluto de uma chamada pública (limite já validado na config).
    fn new_deadline(&self) -> Instant {
        Instant::now() + self.inner.total_timeout
    }

    /// Slot + requisição + corpo sob o prazo absoluto recebido.
    async fn execute(
        &self,
        deadline: Instant,
        method: Method,
        path: &str,
        authenticated: bool,
        body: Option<Vec<u8>>,
    ) -> Result<Reply, SqldAdminError> {
        tokio::time::timeout_at(
            deadline,
            self.execute_inner(method, path, authenticated, body),
        )
        .await
        .map_err(|_| SqldAdminError::Timeout)?
    }

    async fn execute_inner(
        &self,
        method: Method,
        path: &str,
        authenticated: bool,
        body: Option<Vec<u8>>,
    ) -> Result<Reply, SqldAdminError> {
        let _permit = self
            .inner
            .slot
            .acquire()
            .await
            .map_err(|_| SqldAdminError::Transport)?;
        let mut url = self.inner.origin.clone();
        url.set_path(path);
        let mut request = self.inner.client.request(method, url);
        if authenticated {
            let mut value = HeaderValue::from_str(&format!("Basic {}", self.inner.key.as_str()))
                .map_err(|_| SqldAdminError::InvalidKey)?;
            value.set_sensitive(true);
            request = request.header(AUTHORIZATION, value);
        }
        if let Some(body) = body {
            request = request.header(CONTENT_TYPE, "application/json").body(body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| SqldAdminError::Transport)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(SqldAdminError::Redirect);
        }
        if !status.is_success() {
            return Ok(Reply {
                status,
                body: Vec::new(),
            });
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_ADMIN_RESPONSE_BYTES as u64)
        {
            return Err(SqldAdminError::ResponseTooLarge);
        }
        let mut buf = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| SqldAdminError::Transport)?
        {
            match buf.len().checked_add(chunk.len()) {
                Some(total) if total <= MAX_ADMIN_RESPONSE_BYTES => {}
                _ => return Err(SqldAdminError::ResponseTooLarge),
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(Reply { status, body: buf })
    }
}

fn status_error(status: StatusCode) -> SqldAdminError {
    if status == StatusCode::UNAUTHORIZED {
        SqldAdminError::Unauthorized
    } else if status.is_redirection() {
        SqldAdminError::Redirect
    } else {
        SqldAdminError::UnexpectedStatus
    }
}

fn config_path(namespace: &str) -> String {
    format!("/v1/namespaces/{namespace}/config")
}

fn parse_origin(origin: &str) -> Result<Url, SqldAdminError> {
    let url = Url::parse(origin).map_err(|_| SqldAdminError::InvalidOrigin)?;
    let ok = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|h| !h.is_empty())
        && matches!(url.path(), "" | "/")
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none();
    if ok {
        Ok(url)
    } else {
        Err(SqldAdminError::InvalidOrigin)
    }
}

/// `edger_` + 64 hex minúsculos (namespace derivado pelo registry).
fn validate_namespace(namespace: &str) -> Result<(), SqldAdminError> {
    let ok = namespace.strip_prefix(NAMESPACE_PREFIX).is_some_and(|hex| {
        hex.len() == NAMESPACE_HEX_LEN
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if ok {
        Ok(())
    } else {
        Err(SqldAdminError::InvalidNamespace)
    }
}

/// Interpreta o campo `jwt_key` remoto: componente `URL_SAFE_NO_PAD` de
/// exatos 32 bytes, ou UM bloco PEM `PUBLIC KEY` SPKI Ed25519 (RFC 8410).
/// Qualquer outra coisa (vazio, curto, longo, chave privada, vários blocos,
/// outro algoritmo, malformado, > 8 KiB) é [`SqldRemoteKey::Invalid`].
pub fn parse_remote_public_key(field: &str) -> SqldRemoteKey {
    if field.is_empty() || field.len() > MAX_REMOTE_KEY_FIELD_BYTES {
        return SqldRemoteKey::Invalid;
    }
    let parsed = if field.starts_with("-----BEGIN") {
        parse_spki_pem(field)
    } else {
        parse_component(field)
    };
    parsed.map_or(SqldRemoteKey::Invalid, SqldRemoteKey::Single)
}

fn parse_component(field: &str) -> Option<[u8; PUBLIC_KEY_LEN]> {
    let bytes = URL_SAFE_NO_PAD.decode(field).ok()?;
    <[u8; PUBLIC_KEY_LEN]>::try_from(bytes.as_slice()).ok()
}

fn parse_spki_pem(field: &str) -> Option<[u8; PUBLIC_KEY_LEN]> {
    // `from_pem` confere o rótulo `PUBLIC KEY` e consome o documento todo.
    let spki = SubjectPublicKeyInfoOwned::from_pem(field).ok()?;
    if spki.algorithm.oid != ED25519_OID || spki.algorithm.parameters.is_some() {
        return None;
    }
    let raw = spki.subject_public_key.as_bytes()?;
    <[u8; PUBLIC_KEY_LEN]>::try_from(raw).ok()
}
