//! Serviço host de provisionamento e tokens SQLD (EDG-54 / 14c).
//!
//! Liga o registry durável ([`crate::sqld_registry`]), o cliente admin
//! ([`crate::sqld_admin`]), o cofre de segredos ([`crate::app_secrets`]) e o
//! backup ([`crate::sqld_backup`]) num serviço LOCAL. Não é API HTTP/CLI nem
//! binding de runtime (fatia 15): quem chama já resolveu o dono (nome integral
//! do worker + namespace RBAC) a partir do índice instalado; este módulo não
//! lê env, não infere dono e nunca recebe namespace/URL/JWT da aplicação.
//!
//! # Fluxo e estados
//!
//! `ensure(owner, quota)`: (1) cofre disponível; (2) dono novo/`Intent` com
//! `SQLD_TOKEN` já gravado (inclusive vazio) → [`SqldProvisionError::
//! TokenConflict`] SEM probe, reserva nem CREATE; (3) quota diferente da
//! reservada → `QuotaConflict` antes de qualquer chamada remota; (4) probe de
//! auth admin; (5) `ensure_intent` (Intent durável ANTES do CREATE); (6) GET
//! de config FRESCO decide: ausente + `Intent` → CREATE (chave/quota/
//! `allow_attach:false`, feito pelo cliente) e novo GET mesmo em 400;
//! ausente + `Proven`/`Ready` → `MissingRemote` (nunca banco vazio
//! substituto); presente → chave decodificada confere por `mark_proven`
//! (divergente) ou `mark_conflict` (ausente/inválida), ambos persistentes;
//! (7) token próprio (semeado com CAS(None), renovado com CAS(valor exato
//! lido)); (8) quota remota por `PRAGMA max_page_count` com o token próprio;
//! (9) `Proven` novo → backup inicial escopado e só então `mark_ready`.
//! Falha em qualquer etapa preserva Intent/Proven, reserva, namespace e
//! cópias; repetir `ensure` retoma. Nunca há POST de config, drop nem rekey.
//!
//! # Garantias e limites (o que o código realmente checa)
//!
//! * Token manual: o cofre NUNCA recebe `set` incondicional; um PUT manual
//!   concorrente vence a comparação do CAS e o serviço falha fechado sem laço.
//!   Janela residual: entre a leitura e o CAS outro PUT pode ocorrer; o CAS o
//!   detecta, não o evita.
//! * O resolver ([`SqldTokenResolver`]) só devolve `SQLD_TOKEN` em `Ready`,
//!   depois de verificar ASSINATURA EdDSA com a pubkey do registry e claims
//!   EXATOS (`exp` inteiro + `p.rw.ns`/`p.ddl.ns` com um único namespace); o
//!   `exp` é conferido manualmente com o relógio injetado SÓ depois da
//!   assinatura. Menos de 1 h restante (inclusive expirado e autenticado)
//!   renova por registry + CAS. Valor ausente/manual/malformado → `None`, sem
//!   substituição e sem recriação (DELETE manual nega o transporte).
//! * Concorrência: locks por dono (provisionamento e token) com posse RAII;
//!   o mapa só guarda donos com uso/espera em curso. Ordem fixa
//!   provisionamento → token; o resolver e o backup só tomam o de token, e
//!   o backup o solta ANTES do dump. O lock de provisionamento do `ensure`
//!   fica preso durante o backup inicial (só do mesmo dono).
//! * SQLite/crypto/cofre rodam em `spawn_blocking` sob admissão limitada; o
//!   permit MOVE para o job e só volta quando o job termina fisicamente, mesmo
//!   se o `await` for cancelado.
//! * Dados: HTTP à origem raiz fixa (nunca 9090), sem proxy/redirect/retry,
//!   prazo único, corpo ≤ 64 KiB. `max_db_size` do admin não prova quota; só
//!   o inteiro exato `quota/4096` do `PRAGMA max_page_count` conta.
//! * Não provado aqui: crash real de processo entre etapas (os testes usam
//!   falhas de transporte e reconstrução durável explícita), coordenação
//!   entre hosts/processos fora do SQLite IMMEDIATE do registry.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use edger_core::secrets::AppSecretResolver;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;
use tokio::sync::{OwnedMutexGuard, Semaphore};
use zeroize::Zeroizing;

use crate::app_secrets::AppSecretVault;
use crate::sqld_admin::{SqldAdminClient, SqldAdminError, SqldNamespaceConfig, SqldRemoteKey};
use crate::sqld_backup::{BackupArtifact, SqldBackupError, SqldBackupStore};
use crate::sqld_registry::{
    SqldProvisionRegistry, SqldRegistryError, SqldRegistryOwner, SqldRegistryRecord,
    SqldRegistryState, MIN_QUOTA_BYTES, QUOTA_ALIGN_BYTES,
};
use crate::sqld_signer::PUBLIC_KEY_LEN;

/// Nome público do segredo gerenciado (contrato EDG-54).
pub const SQLD_TOKEN_SECRET: &str = "SQLD_TOKEN";
/// TTL do token próprio (12 h).
pub const TOKEN_TTL_SECONDS: u64 = 12 * 3600;
/// Renovar quando restar MENOS que isto (1 h).
pub const TOKEN_RENEW_BEFORE_SECONDS: i64 = 3600;
/// Prazo único e teto do pedido de dados.
pub const DATA_TIMEOUT: Duration = Duration::from_secs(15);
/// Teto do corpo da resposta de dados.
pub const MAX_DATA_RESPONSE_BYTES: usize = 64 * 1024;
/// Jobs bloqueantes simultâneos (SQLite/crypto/cofre), padrão.
pub const DEFAULT_MAX_BLOCKING: usize = 4;
/// Espera máxima por admissão antes de `Busy`.
pub const ADMISSION_WAIT: Duration = Duration::from_secs(15);
const PAGE_BYTES: u64 = QUOTA_ALIGN_BYTES;
const MAX_JWT_BYTES: usize = 16 * 1024;

/// Erros estáticos: nunca carregam token, chave, corpo, URL nem nome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldProvisionError {
    /// Cofre sem chave mestra: o serviço não opera.
    Unavailable,
    InvalidQuota,
    InvalidConfig,
    InvalidOrigin,
    /// Já existe valor (manual/estrangeiro/vazio) em `SQLD_TOKEN`.
    TokenConflict,
    /// `SQLD_TOKEN` ausente onde só o `ensure` explícito pode semear.
    TokenMissing,
    QuotaConflict,
    BudgetExceeded,
    /// Registro em `Conflict` ou chave remota divergente/ausente/inválida.
    KeyConflict,
    /// Proven/Ready sem namespace remoto: sem recriação.
    MissingRemote,
    /// CREATE recusado/ineficaz e o GET seguinte não encontrou o namespace.
    CreateNotVisible,
    NotFound,
    NotReady,
    /// Quota remota diferente de `quota/4096` páginas.
    QuotaMismatch,
    DataTimeout,
    DataTransport,
    DataStatus,
    DataInvalid,
    DataTooLarge,
    /// Admissão de jobs bloqueantes esgotada dentro do prazo.
    Busy,
    /// Job bloqueante abortado/paniquado ou semáforo fechado.
    Internal,
    Vault,
    Registry(SqldRegistryError),
    Admin(SqldAdminError),
    Backup(SqldBackupError),
}

impl fmt::Display for SqldProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "sqld provision: secret vault unavailable",
            Self::InvalidQuota => "sqld provision: invalid quota",
            Self::InvalidConfig => "sqld provision: invalid service config",
            Self::InvalidOrigin => "sqld provision: invalid data origin",
            Self::TokenConflict => "sqld provision: SQLD_TOKEN already holds a foreign value",
            Self::TokenMissing => "sqld provision: SQLD_TOKEN is absent",
            Self::QuotaConflict => "sqld provision: quota differs from the reserved one",
            Self::BudgetExceeded => "sqld provision: managed budget exceeded",
            Self::KeyConflict => "sqld provision: namespace key conflict",
            Self::MissingRemote => "sqld provision: remote namespace is missing",
            Self::CreateNotVisible => "sqld provision: created namespace is not visible",
            Self::NotFound => "sqld provision: owner not provisioned",
            Self::NotReady => "sqld provision: owner is not ready",
            Self::QuotaMismatch => "sqld provision: remote quota mismatch",
            Self::DataTimeout => "sqld provision: data deadline exceeded",
            Self::DataTransport => "sqld provision: data transport error",
            Self::DataStatus => "sqld provision: unexpected data status",
            Self::DataInvalid => "sqld provision: invalid data response",
            Self::DataTooLarge => "sqld provision: data response too large",
            Self::Busy => "sqld provision: blocking admission exhausted",
            Self::Internal => "sqld provision: internal task failure",
            Self::Vault => "sqld provision: secret vault error",
            Self::Registry(_) => "sqld provision: registry error",
            Self::Admin(_) => "sqld provision: admin error",
            Self::Backup(_) => "sqld provision: backup error",
        })
    }
}

impl std::error::Error for SqldProvisionError {}

impl From<SqldRegistryError> for SqldProvisionError {
    fn from(value: SqldRegistryError) -> Self {
        match value {
            SqldRegistryError::QuotaConflict => Self::QuotaConflict,
            SqldRegistryError::BudgetExceeded => Self::BudgetExceeded,
            SqldRegistryError::Conflict | SqldRegistryError::PublicKeyMismatch => Self::KeyConflict,
            SqldRegistryError::NotFound => Self::NotFound,
            other => Self::Registry(other),
        }
    }
}

impl From<SqldAdminError> for SqldProvisionError {
    fn from(value: SqldAdminError) -> Self {
        Self::Admin(value)
    }
}

impl From<SqldBackupError> for SqldProvisionError {
    fn from(value: SqldBackupError) -> Self {
        Self::Backup(value)
    }
}

/// Relógio injetável (produção UTC; testes controlados).
pub trait SqldClock: Send + Sync {
    fn now_unix(&self) -> i64;
}

/// Relógio UTC de produção.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqldSystemClock;

impl SqldClock for SqldSystemClock {
    fn now_unix(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

/// Dono confiável: nome integral do worker + namespace RBAC do host.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SqldOwner {
    worker: String,
    rbac_namespace: Option<String>,
}

impl SqldOwner {
    pub fn new(worker: impl Into<String>, rbac_namespace: Option<String>) -> Self {
        Self {
            worker: worker.into(),
            rbac_namespace,
        }
    }

    pub fn worker(&self) -> &str {
        &self.worker
    }

    fn registry(&self) -> SqldRegistryOwner<'_> {
        SqldRegistryOwner {
            worker: &self.worker,
            rbac_namespace: self.rbac_namespace.as_deref(),
        }
    }
}

impl fmt::Debug for SqldOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldOwner").finish_non_exhaustive()
    }
}

/// Metadata do vínculo: sem token, chave nem envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqldProvisionMetadata {
    pub state: SqldRegistryState,
    pub sql_namespace: String,
    pub quota_bytes: u64,
}

impl From<&SqldRegistryRecord> for SqldProvisionMetadata {
    fn from(record: &SqldRegistryRecord) -> Self {
        Self {
            state: record.state,
            sql_namespace: record.sql_namespace.clone(),
            quota_bytes: record.quota_bytes,
        }
    }
}

/// Resultado do `ensure`: metadata + backup inicial (só quando este
/// `ensure` o criou; `Ready` repetido não cria outro).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqldEnsureOutcome {
    pub metadata: SqldProvisionMetadata,
    pub initial_backup: Option<BackupArtifact>,
}

/// Parâmetros do serviço.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqldProvisionConfig {
    /// Jobs bloqueantes simultâneos (≥ 1).
    pub max_blocking: usize,
    /// Prazo único do pedido de dados (positivo, até [`DATA_TIMEOUT`]).
    pub data_timeout: Duration,
    /// Espera por admissão (positiva).
    pub admission_wait: Duration,
}

impl Default for SqldProvisionConfig {
    fn default() -> Self {
        Self {
            max_blocking: DEFAULT_MAX_BLOCKING,
            data_timeout: DATA_TIMEOUT,
            admission_wait: ADMISSION_WAIT,
        }
    }
}

// ───────────────────────── locks por dono (RAII) ─────────────────────────

struct LockEntry {
    mutex: Arc<tokio::sync::Mutex<()>>,
    refs: usize,
}

#[derive(Default)]
struct OwnerLocks {
    map: Mutex<HashMap<SqldOwner, LockEntry>>,
}

/// Contagem de uso (dono em espera ou segurando). O último a sair remove a
/// entrada: o mapa não cresce com donos mortos, nem com esperas canceladas.
struct Ticket {
    locks: Arc<OwnerLocks>,
    key: SqldOwner,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut map = self
            .locks
            .map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = map.get_mut(&self.key) {
            entry.refs -= 1;
            if entry.refs == 0 {
                map.remove(&self.key);
            }
        }
    }
}

/// Posse do lock de um dono. O guard cai ANTES do ticket (ordem dos campos).
struct OwnerGuard {
    _guard: OwnedMutexGuard<()>,
    _ticket: Ticket,
}

impl OwnerLocks {
    async fn acquire(self: &Arc<Self>, owner: &SqldOwner) -> OwnerGuard {
        let mutex = {
            let mut map = self
                .map
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = map.entry(owner.clone()).or_insert_with(|| LockEntry {
                mutex: Arc::new(tokio::sync::Mutex::new(())),
                refs: 0,
            });
            entry.refs += 1;
            Arc::clone(&entry.mutex)
        };
        // O ticket existe antes do await: cancelar a espera o solta.
        let ticket = Ticket {
            locks: Arc::clone(self),
            key: owner.clone(),
        };
        let guard = mutex.lock_owned().await;
        OwnerGuard {
            _guard: guard,
            _ticket: ticket,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

// ───────────────────────── token: inspeção pura ─────────────────────────

/// Resultado da inspeção de um valor do cofre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenInspection {
    /// Autêntico, claims exatos, ≥ 1 h restante.
    Fresh,
    /// Autêntico, claims exatos, < 1 h restante ou já expirado.
    Renew,
    /// Manual/estrangeiro/malformado/forma legada: nunca substituir.
    Invalid,
}

fn exact_ns_object(value: &Value, namespace: &str) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.len() != 1 {
        return false;
    }
    match object.get("ns").and_then(Value::as_array) {
        Some(list) => list.len() == 1 && list[0].as_str() == Some(namespace),
        None => false,
    }
}

/// Verifica a ASSINATURA EdDSA com `public_key` (sem `exp`), depois os claims
/// exatos e só então o `exp` com `now_unix`.
fn inspect_token(
    jwt: &str,
    public_key: &[u8; PUBLIC_KEY_LEN],
    namespace: &str,
    now_unix: i64,
) -> TokenInspection {
    if jwt.is_empty() || jwt.len() > MAX_JWT_BYTES {
        return TokenInspection::Invalid;
    }
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = false;
    validation.validate_nbf = false;
    validation.validate_aud = false;
    validation.required_spec_claims.clear();
    let x = URL_SAFE_NO_PAD.encode(public_key);
    let Ok(key) = DecodingKey::from_ed_components(&x) else {
        return TokenInspection::Invalid;
    };
    let Ok(data) = jsonwebtoken::decode::<Value>(jwt, &key, &validation) else {
        return TokenInspection::Invalid;
    };
    let Some(claims) = data.claims.as_object() else {
        return TokenInspection::Invalid;
    };
    if claims.len() != 2 {
        return TokenInspection::Invalid;
    }
    let Some(p) = claims.get("p").and_then(Value::as_object) else {
        return TokenInspection::Invalid;
    };
    let scoped = p.len() == 2
        && p.get("rw").is_some_and(|v| exact_ns_object(v, namespace))
        && p.get("ddl").is_some_and(|v| exact_ns_object(v, namespace));
    if !scoped {
        return TokenInspection::Invalid;
    }
    let Some(exp) = claims.get("exp").and_then(Value::as_i64).filter(|e| *e > 0) else {
        return TokenInspection::Invalid;
    };
    // `now` pode ser qualquer i64; a subtração é saturada.
    if exp.saturating_sub(now_unix) >= TOKEN_RENEW_BEFORE_SECONDS {
        TokenInspection::Fresh
    } else {
        TokenInspection::Renew
    }
}

/// Estado do `SQLD_TOKEN` no cofre, já inspecionado.
enum TokenState {
    Missing,
    Fresh(Zeroizing<String>),
    Renew(Zeroizing<String>),
    Foreign,
}

// ───────────────────────── cliente de dados (quota) ─────────────────────────

/// Mesma fronteira de `EgressDatabaseGrant`/backup: `http`, host, sem
/// userinfo/query/fragment, caminho `/`, nunca a porta 9090.
fn parse_data_origin(origin: &str) -> Result<Url, SqldProvisionError> {
    let invalid = SqldProvisionError::InvalidOrigin;
    let url = Url::parse(origin).map_err(|_| invalid)?;
    if url.scheme() != "http"
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() == Some(9090)
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(invalid);
    }
    Ok(url)
}

struct DataClient {
    client: Client,
    pipeline_url: Url,
    timeout: Duration,
}

impl DataClient {
    fn new(origin: &Url, timeout: Duration) -> Result<Self, SqldProvisionError> {
        let client = Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| SqldProvisionError::InvalidConfig)?;
        let mut pipeline_url = origin.clone();
        pipeline_url.set_path("/v2/pipeline");
        Ok(Self {
            client,
            pipeline_url,
            timeout,
        })
    }

    /// `PRAGMA max_page_count` com o token do app e o namespace imposto.
    /// Só `200` + primeiro resultado `ok` + UMA célula inteira vale.
    async fn max_page_count(
        &self,
        namespace: &str,
        token: &str,
    ) -> Result<i64, SqldProvisionError> {
        let mut bearer = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| SqldProvisionError::DataInvalid)?;
        bearer.set_sensitive(true);
        let ns_header = HeaderValue::from_str(&STANDARD_NO_PAD.encode(namespace.as_bytes()))
            .map_err(|_| SqldProvisionError::DataInvalid)?;
        let body = serde_json::to_vec(&serde_json::json!({
            "requests": [
                { "type": "execute", "stmt": { "sql": "PRAGMA max_page_count" } },
                { "type": "close" },
            ]
        }))
        .map_err(|_| SqldProvisionError::DataInvalid)?;
        let call = async {
            let mut response = self
                .client
                .post(self.pipeline_url.clone())
                .header(AUTHORIZATION, bearer)
                .header("x-namespace-bin", ns_header)
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await
                .map_err(|e| {
                    if e.is_timeout() {
                        SqldProvisionError::DataTimeout
                    } else {
                        SqldProvisionError::DataTransport
                    }
                })?;
            if response.status() != StatusCode::OK {
                return Err(SqldProvisionError::DataStatus);
            }
            if response
                .content_length()
                .is_some_and(|len| len > MAX_DATA_RESPONSE_BYTES as u64)
            {
                return Err(SqldProvisionError::DataTooLarge);
            }
            let mut buf: Vec<u8> = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| SqldProvisionError::DataTransport)?
            {
                if buf.len().saturating_add(chunk.len()) > MAX_DATA_RESPONSE_BYTES {
                    return Err(SqldProvisionError::DataTooLarge);
                }
                buf.extend_from_slice(&chunk);
            }
            parse_single_integer(&buf)
        };
        tokio::time::timeout(self.timeout, call)
            .await
            .map_err(|_| SqldProvisionError::DataTimeout)?
    }
}

/// Exatamente `results[0].type == "ok"` com uma linha e uma célula
/// `{"type":"integer","value":"<n>"}`. Um `200` com erro Hrana não passa.
fn parse_single_integer(body: &[u8]) -> Result<i64, SqldProvisionError> {
    let invalid = SqldProvisionError::DataInvalid;
    let value: Value = serde_json::from_slice(body).map_err(|_| invalid)?;
    let first = value.pointer("/results/0").ok_or(invalid)?;
    if first.get("type").and_then(Value::as_str) != Some("ok") {
        return Err(invalid);
    }
    let rows = first
        .pointer("/response/result/rows")
        .and_then(Value::as_array)
        .ok_or(invalid)?;
    let [row] = rows.as_slice() else {
        return Err(invalid);
    };
    let [cell] = row.as_array().ok_or(invalid)?.as_slice() else {
        return Err(invalid);
    };
    if cell.get("type").and_then(Value::as_str) != Some("integer") {
        return Err(invalid);
    }
    cell.get("value")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<i64>().ok())
        .ok_or(invalid)
}

// ───────────────────────── serviço ─────────────────────────

struct Inner {
    registry: Arc<SqldProvisionRegistry>,
    vault: Arc<AppSecretVault>,
    admin: SqldAdminClient,
    backup: SqldBackupStore,
    data: DataClient,
    clock: Arc<dyn SqldClock>,
    admission: Arc<Semaphore>,
    admission_wait: Duration,
    provision_locks: Arc<OwnerLocks>,
    token_locks: Arc<OwnerLocks>,
}

/// Serviço host. Clones compartilham tudo. `Debug` não mostra nada.
#[derive(Clone)]
pub struct SqldProvisionService {
    inner: Arc<Inner>,
}

impl fmt::Debug for SqldProvisionService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldProvisionService")
            .finish_non_exhaustive()
    }
}

impl SqldProvisionService {
    pub fn new(
        registry: Arc<SqldProvisionRegistry>,
        vault: Arc<AppSecretVault>,
        admin: SqldAdminClient,
        backup: SqldBackupStore,
        data_origin: &str,
        clock: Arc<dyn SqldClock>,
    ) -> Result<Self, SqldProvisionError> {
        Self::with_config(
            registry,
            vault,
            admin,
            backup,
            data_origin,
            clock,
            SqldProvisionConfig::default(),
        )
    }

    pub fn with_config(
        registry: Arc<SqldProvisionRegistry>,
        vault: Arc<AppSecretVault>,
        admin: SqldAdminClient,
        backup: SqldBackupStore,
        data_origin: &str,
        clock: Arc<dyn SqldClock>,
        config: SqldProvisionConfig,
    ) -> Result<Self, SqldProvisionError> {
        if config.max_blocking == 0
            || config.max_blocking > Semaphore::MAX_PERMITS
            || config.data_timeout.is_zero()
            || config.data_timeout > DATA_TIMEOUT
            || config.admission_wait.is_zero()
        {
            return Err(SqldProvisionError::InvalidConfig);
        }
        let origin = parse_data_origin(data_origin)?;
        let data = DataClient::new(&origin, config.data_timeout)?;
        Ok(Self {
            inner: Arc::new(Inner {
                registry,
                vault,
                admin,
                backup,
                data,
                clock,
                admission: Arc::new(Semaphore::new(config.max_blocking)),
                admission_wait: config.admission_wait,
                provision_locks: Arc::new(OwnerLocks::default()),
                token_locks: Arc::new(OwnerLocks::default()),
            }),
        })
    }

    /// Resolver de segredos vinculado ao dono (o namespace RBAC fica
    /// capturado). Outros nomes de segredo delegam ao cofre; outro worker
    /// recebe `None`.
    pub fn resolver_for(&self, owner: SqldOwner) -> Arc<SqldTokenResolver> {
        Arc::new(SqldTokenResolver {
            service: self.clone(),
            owner: Arc::new(owner),
        })
    }

    /// Resolver de segredos para um spawn admitido SEM vínculo (modo legado).
    /// Síncrono e sem I/O. Só `SQLD_TOKEN` difere de [`Self::resolver_for`]:
    /// lê o valor do cofre PRIMEIRO e DEPOIS consulta o registry; devolve o
    /// valor só se o registry confirmar ausência global para o nome. Presença ou
    /// erro → `None`. Nunca gera, renova, semeia nem guarda valores. Outros
    /// nomes delegam ao cofre; outro worker recebe `None`.
    pub fn resolver_for_legacy_spawn(&self, owner: SqldOwner) -> Arc<dyn AppSecretResolver> {
        Arc::new(SqldLegacySpawnResolver {
            service: self.clone(),
            owner: Arc::new(owner),
        })
    }

    /// Metadata do vínculo (sem token/chave). `None` = dono sem registro.
    pub async fn metadata(
        &self,
        owner: &SqldOwner,
    ) -> Result<Option<SqldProvisionMetadata>, SqldProvisionError> {
        let record = self.registry_get(owner).await?;
        Ok(record.as_ref().map(SqldProvisionMetadata::from))
    }

    /// Assegura o vínculo (ver o fluxo no topo do módulo). Idempotente.
    pub async fn ensure(
        &self,
        owner: &SqldOwner,
        quota_bytes: u64,
    ) -> Result<SqldEnsureOutcome, SqldProvisionError> {
        if quota_bytes < MIN_QUOTA_BYTES
            || !quota_bytes.is_multiple_of(QUOTA_ALIGN_BYTES)
            || i64::try_from(quota_bytes).is_err()
        {
            return Err(SqldProvisionError::InvalidQuota);
        }
        if !self.inner.vault.is_available() {
            return Err(SqldProvisionError::Unavailable);
        }
        let _provision = self.inner.provision_locks.acquire(owner).await;

        // Estado atual: nada remoto ainda.
        let existing = self.registry_get(owner).await?;
        match &existing {
            Some(rec) if rec.state == SqldRegistryState::Conflict => {
                return Err(SqldProvisionError::KeyConflict)
            }
            Some(rec) if rec.quota_bytes != quota_bytes => {
                return Err(SqldProvisionError::QuotaConflict)
            }
            Some(rec) if rec.state != SqldRegistryState::Intent => {}
            // Dono novo ou ainda Intent: nenhum token pode existir (mesmo
            // vazio) antes de provar o namespace.
            _ => {
                let held = self.vault_has_value(owner).await?;
                if held {
                    return Err(SqldProvisionError::TokenConflict);
                }
            }
        }

        self.inner.admin.probe_admin_auth().await?;
        let record = self.registry_ensure_intent(owner, quota_bytes).await?;
        let allow_create = record.state == SqldRegistryState::Intent;
        let proven = self.prove_remote(owner, &record, allow_create).await?;

        let token = {
            let _token_lock = self.inner.token_locks.acquire(owner).await;
            self.ensure_token_locked(owner, &proven, true).await?
        };
        let pages = self
            .inner
            .data
            .max_page_count(&proven.sql_namespace, &token)
            .await?;
        if u64::try_from(pages).ok() != Some(proven.quota_bytes / PAGE_BYTES) {
            return Err(SqldProvisionError::QuotaMismatch);
        }

        let mut initial_backup = None;
        if proven.state == SqldRegistryState::Proven {
            let artifact = self
                .inner
                .backup
                .create_for_quota(
                    &proven.sql_namespace,
                    Zeroizing::new((*token).clone()),
                    proven.quota_bytes,
                )
                .await?;
            initial_backup = Some(artifact);
            self.registry_mark_ready(owner).await?;
        }
        drop(token);
        let current = self
            .registry_get(owner)
            .await?
            .ok_or(SqldProvisionError::NotFound)?;
        Ok(SqldEnsureOutcome {
            metadata: SqldProvisionMetadata::from(&current),
            initial_backup,
        })
    }

    /// Backup sob demanda: exige `Ready`, prova o namespace remoto antes e
    /// usa a credencial própria atual. Devolve só metadata do arquivo. O lock
    /// de token é solto ANTES do dump e não há lock de provisionamento.
    pub async fn backup(&self, owner: &SqldOwner) -> Result<BackupArtifact, SqldProvisionError> {
        if !self.inner.vault.is_available() {
            return Err(SqldProvisionError::Unavailable);
        }
        let record = self
            .registry_get(owner)
            .await?
            .ok_or(SqldProvisionError::NotFound)?;
        match record.state {
            SqldRegistryState::Ready => {}
            SqldRegistryState::Conflict => return Err(SqldProvisionError::KeyConflict),
            _ => return Err(SqldProvisionError::NotReady),
        }
        let proven = self.prove_remote(owner, &record, false).await?;
        let token = {
            let _token_lock = self.inner.token_locks.acquire(owner).await;
            self.ensure_token_locked(owner, &proven, false).await?
        };
        let artifact = self
            .inner
            .backup
            .create_for_quota(&proven.sql_namespace, token, proven.quota_bytes)
            .await?;
        Ok(artifact)
    }

    // ─── etapas ───

    /// GET fresco decide. `allow_create` só vale para `Intent`.
    async fn prove_remote(
        &self,
        owner: &SqldOwner,
        record: &SqldRegistryRecord,
        allow_create: bool,
    ) -> Result<SqldRegistryRecord, SqldProvisionError> {
        let admin = &self.inner.admin;
        let mut config = admin.read_config(&record.sql_namespace).await?;
        if config == SqldNamespaceConfig::Missing {
            if !allow_create {
                return Err(SqldProvisionError::MissingRemote);
            }
            // O resultado (200/400) não prova nada: só o GET seguinte.
            admin.create_namespace(record).await?;
            config = admin.read_config(&record.sql_namespace).await?;
            if config == SqldNamespaceConfig::Missing {
                return Err(SqldProvisionError::CreateNotVisible);
            }
        }
        match config {
            SqldNamespaceConfig::Present {
                key: SqldRemoteKey::Single(observed),
            } => {
                let registry = Arc::clone(&self.inner.registry);
                let owner = owner.clone();
                self.offload(move || registry.mark_proven(&owner.registry(), observed))
                    .await?
                    .map_err(SqldProvisionError::from)
            }
            SqldNamespaceConfig::Present { .. } => {
                let registry = Arc::clone(&self.inner.registry);
                let owner = owner.clone();
                self.offload(move || registry.mark_conflict(&owner.registry()))
                    .await?
                    .map_err(SqldProvisionError::from)?;
                Err(SqldProvisionError::KeyConflict)
            }
            SqldNamespaceConfig::Missing => Err(SqldProvisionError::MissingRemote),
        }
    }

    /// Com o lock de token do dono já tomado: devolve um token próprio FRESCO.
    /// `seed` = só o `ensure` explícito semeia valor ausente (CAS(None)).
    async fn ensure_token_locked(
        &self,
        owner: &SqldOwner,
        record: &SqldRegistryRecord,
        seed: bool,
    ) -> Result<Zeroizing<String>, SqldProvisionError> {
        match self.read_token_state(owner, record).await? {
            TokenState::Fresh(token) => Ok(token),
            TokenState::Foreign => Err(SqldProvisionError::TokenConflict),
            TokenState::Missing if !seed => Err(SqldProvisionError::TokenMissing),
            TokenState::Missing => self.mint_and_store(owner, None).await,
            TokenState::Renew(old) => self.mint_and_store(owner, Some(old)).await,
        }
    }

    /// Assina pelo registry e grava por CAS (`None` ou o valor exato lido).
    /// CAS perdida = falha fechada, sem laço.
    async fn mint_and_store(
        &self,
        owner: &SqldOwner,
        expected: Option<Zeroizing<String>>,
    ) -> Result<Zeroizing<String>, SqldProvisionError> {
        let now = self.inner.clock.now_unix();
        let registry = Arc::clone(&self.inner.registry);
        let vault = Arc::clone(&self.inner.vault);
        let owner = owner.clone();
        let applied = self
            .offload(
                move || -> Result<Option<Zeroizing<String>>, SqldProvisionError> {
                    let signed = registry
                        .sign_scoped_token(&owner.registry(), now, TOKEN_TTL_SECONDS)
                        .map_err(SqldProvisionError::from)?;
                    let jwt = Zeroizing::new(signed.expose_jwt().to_owned());
                    let swapped = vault
                        .compare_and_set(
                            &owner.worker,
                            SQLD_TOKEN_SECRET,
                            expected.as_ref().map(|v| v.as_str()),
                            &jwt,
                        )
                        .map_err(|_| SqldProvisionError::Vault)?;
                    Ok(swapped.then_some(jwt))
                },
            )
            .await??;
        applied.ok_or(SqldProvisionError::TokenConflict)
    }

    /// Lê e inspeciona o valor do cofre (SQLite + Ed25519 offloadados).
    async fn read_token_state(
        &self,
        owner: &SqldOwner,
        record: &SqldRegistryRecord,
    ) -> Result<TokenState, SqldProvisionError> {
        let now = self.inner.clock.now_unix();
        let vault = Arc::clone(&self.inner.vault);
        let owner = owner.clone();
        let public_key = record.public_key;
        let namespace = record.sql_namespace.clone();
        self.offload(move || match vault.get(&owner.worker, SQLD_TOKEN_SECRET) {
            None => TokenState::Missing,
            Some(value) => match inspect_token(&value, &public_key, &namespace, now) {
                TokenInspection::Fresh => TokenState::Fresh(Zeroizing::new((*value).clone())),
                TokenInspection::Renew => TokenState::Renew(Zeroizing::new((*value).clone())),
                TokenInspection::Invalid => TokenState::Foreign,
            },
        })
        .await
    }

    /// Presença do nome no cofre, FALÍVEL e independente de decifragem:
    /// `AppSecretVault::list` lê só nomes/datas (SELECT em `app_secrets`),
    /// então um valor existente (inclusive vazio, ou cifrado sob outra
    /// master) conta como presente; falha de storage/cofre indisponível é
    /// `Vault` (fecha antes de qualquer mutação), nunca "ausente".
    /// Janela: é uma leitura; um PUT concorrente depois dela é pego pelo
    /// CAS(None) da semeadura.
    async fn vault_has_value(&self, owner: &SqldOwner) -> Result<bool, SqldProvisionError> {
        let vault = Arc::clone(&self.inner.vault);
        let owner = owner.clone();
        self.offload(move || {
            vault
                .list(&owner.worker)
                .map(|names| names.iter().any(|meta| meta.name == SQLD_TOKEN_SECRET))
                .map_err(|_| SqldProvisionError::Vault)
        })
        .await?
    }

    async fn registry_get(
        &self,
        owner: &SqldOwner,
    ) -> Result<Option<SqldRegistryRecord>, SqldProvisionError> {
        let registry = Arc::clone(&self.inner.registry);
        let owner = owner.clone();
        self.offload(move || registry.get(&owner.registry()))
            .await?
            .map_err(SqldProvisionError::from)
    }

    async fn registry_has_worker_name(&self, worker: &str) -> Result<bool, SqldProvisionError> {
        let registry = Arc::clone(&self.inner.registry);
        let worker = worker.to_owned();
        self.offload(move || registry.has_worker_name(&worker))
            .await?
            .map_err(SqldProvisionError::from)
    }

    async fn registry_ensure_intent(
        &self,
        owner: &SqldOwner,
        quota_bytes: u64,
    ) -> Result<SqldRegistryRecord, SqldProvisionError> {
        let registry = Arc::clone(&self.inner.registry);
        let owner = owner.clone();
        self.offload(move || registry.ensure_intent(&owner.registry(), quota_bytes))
            .await?
            .map_err(SqldProvisionError::from)
    }

    async fn registry_mark_ready(&self, owner: &SqldOwner) -> Result<(), SqldProvisionError> {
        let registry = Arc::clone(&self.inner.registry);
        let owner = owner.clone();
        self.offload(move || registry.mark_ready(&owner.registry()))
            .await?
            .map(|_| ())
            .map_err(SqldProvisionError::from)
    }

    /// Job bloqueante sob admissão limitada. O permit MOVE para o job e só
    /// volta quando ele termina fisicamente, mesmo se este `await` for
    /// cancelado (o `JoinHandle` solto não aborta o job).
    async fn offload<T, F>(&self, job: F) -> Result<T, SqldProvisionError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let permit = tokio::time::timeout(
            self.inner.admission_wait,
            Arc::clone(&self.inner.admission).acquire_owned(),
        )
        .await
        .map_err(|_| SqldProvisionError::Busy)?
        .map_err(|_| SqldProvisionError::Internal)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            job()
        })
        .await
        .map_err(|_| SqldProvisionError::Internal)
    }
}

// ───────────────────────── resolver ─────────────────────────

/// Resolver de segredos de UM dono. Só `SQLD_TOKEN` tem lógica própria.
pub struct SqldTokenResolver {
    service: SqldProvisionService,
    owner: Arc<SqldOwner>,
}

impl fmt::Debug for SqldTokenResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldTokenResolver").finish_non_exhaustive()
    }
}

impl SqldTokenResolver {
    async fn resolve_token(&self) -> Option<String> {
        let service = &self.service;
        let owner = &*self.owner;
        let record = match service.registry_get(owner).await {
            Ok(record) => record,
            Err(_) => return None,
        };
        let Some(record) = record else {
            // Sem registro: token manual legado, SOMENTE se nenhum namespace
            // tem registro para o nome do worker (mesma regra do spawn legado;
            // cofre primeiro, registry depois). O cofre é por nome de worker,
            // então ler direto entregaria o token de um dono Ready irmão.
            return service
                .resolver_for_legacy_spawn(owner.clone())
                .resolve_secret(owner.worker(), SQLD_TOKEN_SECRET)
                .await;
        };
        if record.state != SqldRegistryState::Ready {
            return None;
        }
        // Caminho rápido sem lock.
        match service.read_token_state(owner, &record).await.ok()? {
            TokenState::Fresh(token) => return Some((*token).clone()),
            TokenState::Missing | TokenState::Foreign => return None,
            TokenState::Renew(_) => {}
        }
        // Renovação serializada por dono; relê sob o lock (outro resolver
        // pode ter renovado) e nunca semeia valor ausente.
        let _guard = service.inner.token_locks.acquire(owner).await;
        match service.read_token_state(owner, &record).await.ok()? {
            TokenState::Fresh(token) => Some((*token).clone()),
            TokenState::Missing | TokenState::Foreign => None,
            TokenState::Renew(old) => service
                .mint_and_store(owner, Some(old))
                .await
                .ok()
                .map(|token| (*token).clone()),
        }
    }
}

#[async_trait]
impl AppSecretResolver for SqldTokenResolver {
    async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
        if worker != self.owner.worker() {
            return None;
        }
        if name != SQLD_TOKEN_SECRET {
            return self.service.inner.vault.resolve_secret(worker, name).await;
        }
        self.resolve_token().await
    }
}

/// Resolver do spawn legado (ver [`SqldProvisionService::
/// resolver_for_legacy_spawn`]). Sem `Debug` e sem cache.
struct SqldLegacySpawnResolver {
    service: SqldProvisionService,
    owner: Arc<SqldOwner>,
}

#[async_trait]
impl AppSecretResolver for SqldLegacySpawnResolver {
    async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
        let service = &self.service;
        let owner = &*self.owner;
        if worker != owner.worker() {
            return None;
        }
        let value = service.inner.vault.resolve_secret(worker, name).await;
        if name != SQLD_TOKEN_SECRET {
            return value;
        }
        // Ordem: cofre primeiro, registry depois. Um token gravado por um
        // `ensure` concorrente é precedido por Intent durável, logo esta
        // leitura posterior encontra o nome em qualquer namespace/estado e nega o valor.
        match service.registry_has_worker_name(worker).await {
            Ok(false) => value,
            Ok(true) | Err(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

    const NS: &str = "edger_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const NOW: i64 = 1_800_000_000;

    struct TestKey {
        pkcs8: Vec<u8>,
        public: [u8; 32],
    }

    fn test_key() -> TestKey {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .unwrap()
            .as_ref()
            .to_vec();
        let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).unwrap();
        let public: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
        TestKey { pkcs8, public }
    }

    fn sign(key: &TestKey, claims: &Value, alg: Algorithm) -> String {
        let encoding = match alg {
            Algorithm::EdDSA => jsonwebtoken::EncodingKey::from_ed_der(&key.pkcs8),
            _ => jsonwebtoken::EncodingKey::from_secret(b"secret"),
        };
        jsonwebtoken::encode(&jsonwebtoken::Header::new(alg), claims, &encoding).unwrap()
    }

    fn good(exp: i64) -> Value {
        json!({"exp": exp, "p": {"rw": {"ns": [NS]}, "ddl": {"ns": [NS]}}})
    }

    #[test]
    fn inspect_accepts_exact_claims_and_classifies_expiry_after_the_signature() {
        let key = test_key();
        let check = |claims: Value| {
            inspect_token(&sign(&key, &claims, Algorithm::EdDSA), &key.public, NS, NOW)
        };
        assert_eq!(check(good(NOW + 12 * 3600)), TokenInspection::Fresh);
        assert_eq!(check(good(NOW + 3600)), TokenInspection::Fresh);
        assert_eq!(check(good(NOW + 3599)), TokenInspection::Renew);
        assert_eq!(check(good(NOW)), TokenInspection::Renew);
        // autenticado mas expirado: renovar, nunca "inválido"
        assert_eq!(check(good(NOW - 100)), TokenInspection::Renew);
        assert_eq!(check(good(1)), TokenInspection::Renew);
    }

    #[test]
    fn inspect_rejects_every_hostile_variant() {
        let key = test_key();
        let other = test_key();
        let ok_exp = NOW + 12 * 3600;
        let eddsa = |claims: Value| sign(&key, &claims, Algorithm::EdDSA);
        let cases: Vec<(&str, String)> = vec![
            (
                "foreign signer",
                sign(&other, &good(ok_exp), Algorithm::EdDSA),
            ),
            ("hs256", sign(&key, &good(ok_exp), Algorithm::HS256)),
            ("not a jwt", "manual-token".to_owned()),
            ("empty", String::new()),
            ("legacy unscoped", eddsa(json!({"exp": ok_exp}))),
            ("legacy a", eddsa(json!({"exp": ok_exp, "a": "rw"}))),
            (
                "missing ddl",
                eddsa(json!({"exp": ok_exp, "p": {"rw": {"ns": [NS]}}})),
            ),
            (
                "missing rw",
                eddsa(json!({"exp": ok_exp, "p": {"ddl": {"ns": [NS]}}})),
            ),
            (
                "other namespace",
                eddsa(
                    json!({"exp": ok_exp, "p": {"rw": {"ns": ["edger_x"]}, "ddl": {"ns": [NS]}}}),
                ),
            ),
            (
                "two namespaces",
                eddsa(
                    json!({"exp": ok_exp, "p": {"rw": {"ns": [NS, "edger_x"]}, "ddl": {"ns": [NS]}}}),
                ),
            ),
            (
                "empty ns list",
                eddsa(json!({"exp": ok_exp, "p": {"rw": {"ns": []}, "ddl": {"ns": [NS]}}})),
            ),
            (
                "ns not a list",
                eddsa(json!({"exp": ok_exp, "p": {"rw": {"ns": NS}, "ddl": {"ns": [NS]}}})),
            ),
            (
                "extra claim",
                eddsa(
                    json!({"exp": ok_exp, "x": 1, "p": {"rw": {"ns": [NS]}, "ddl": {"ns": [NS]}}}),
                ),
            ),
            (
                "extra p key",
                eddsa(
                    json!({"exp": ok_exp, "p": {"rw": {"ns": [NS]}, "ddl": {"ns": [NS]}, "ro": {"ns": [NS]}}}),
                ),
            ),
            (
                "extra rw key",
                eddsa(
                    json!({"exp": ok_exp, "p": {"rw": {"ns": [NS], "tables": ["t"]}, "ddl": {"ns": [NS]}}}),
                ),
            ),
            (
                "no exp",
                eddsa(json!({"p": {"rw": {"ns": [NS]}, "ddl": {"ns": [NS]}}})),
            ),
            (
                "string exp",
                eddsa(
                    json!({"exp": "99999999999", "p": {"rw": {"ns": [NS]}, "ddl": {"ns": [NS]}}}),
                ),
            ),
            (
                "float exp",
                eddsa(json!({"exp": 1900000000.5, "p": {"rw": {"ns": [NS]}, "ddl": {"ns": [NS]}}})),
            ),
            (
                "negative exp",
                eddsa(json!({"exp": -5, "p": {"rw": {"ns": [NS]}, "ddl": {"ns": [NS]}}})),
            ),
            ("array claims", eddsa(json!([1, 2]))),
        ];
        for (label, jwt) in cases {
            assert_eq!(
                inspect_token(&jwt, &key.public, NS, NOW),
                TokenInspection::Invalid,
                "{label}"
            );
        }
        // tampered payload keeps header+signature: signature check fails
        let jwt = eddsa(good(ok_exp));
        let parts: Vec<&str> = jwt.split('.').collect();
        let forged_payload =
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&good(ok_exp + 999_999)).unwrap());
        let forged = format!("{}.{}.{}", parts[0], forged_payload, parts[2]);
        assert_eq!(
            inspect_token(&forged, &key.public, NS, NOW),
            TokenInspection::Invalid
        );
        // control: untouched token is accepted
        assert_eq!(
            inspect_token(&jwt, &key.public, NS, NOW),
            TokenInspection::Fresh
        );
    }

    #[test]
    fn single_integer_parser_accepts_only_one_exact_ok_cell() {
        let ok = |v: &str| {
            json!({"results": [{"type": "ok", "response": {"type": "execute", "result": {
                "rows": [[{"type": "integer", "value": v}]]}}}, {"type": "ok", "response": {"type": "close"}}]})
        };
        assert_eq!(
            parse_single_integer(ok("4096").to_string().as_bytes()),
            Ok(4096)
        );
        let bad: Vec<Value> = vec![
            json!({"results": [{"type": "error", "error": {"message": "secret body", "code": "X"}}]}),
            json!({"results": []}),
            json!({"results": [{"type": "ok", "response": {"result": {"rows": []}}}]}),
            json!({"results": [{"type": "ok", "response": {"result": {"rows": [[{"type":"integer","value":"1"}],[{"type":"integer","value":"1"}]]}}}]}),
            json!({"results": [{"type": "ok", "response": {"result": {"rows": [[{"type":"integer","value":"1"},{"type":"integer","value":"2"}]]}}}]}),
            json!({"results": [{"type": "ok", "response": {"result": {"rows": [[{"type":"text","value":"4096"}]]}}}]}),
            json!({"results": [{"type": "ok", "response": {"result": {"rows": [[{"type":"integer","value":4096}]]}}}]}),
            json!({"results": [{"type": "ok", "response": {"result": {"rows": [[{"type":"integer","value":"4096.5"}]]}}}]}),
            json!({"results": [{"type": "ok", "response": {"result": {"rows": [[{"type":"integer","value":"abc"}]]}}}]}),
            json!([1]),
        ];
        for value in bad {
            assert_eq!(
                parse_single_integer(value.to_string().as_bytes()),
                Err(SqldProvisionError::DataInvalid),
                "{value}"
            );
        }
        assert_eq!(
            parse_single_integer(b"not json"),
            Err(SqldProvisionError::DataInvalid)
        );
    }

    #[test]
    fn data_origin_shares_the_grant_boundary() {
        assert!(parse_data_origin("http://127.0.0.1:8080").is_ok());
        assert!(parse_data_origin("http://sqld.local").is_ok());
        for bad in [
            "https://127.0.0.1:8080",
            "http://127.0.0.1:9090",
            "http://u:p@127.0.0.1:8080",
            "http://127.0.0.1:8080/x",
            "http://127.0.0.1:8080/?q=1",
            "http://127.0.0.1:8080/#f",
            "127.0.0.1:8080",
            "",
        ] {
            assert_eq!(
                parse_data_origin(bad).unwrap_err(),
                SqldProvisionError::InvalidOrigin,
                "{bad}"
            );
        }
    }

    fn owner(name: &str) -> SqldOwner {
        SqldOwner::new(name, Some("@ns".to_owned()))
    }

    #[tokio::test]
    async fn owner_locks_serialize_per_owner_and_drop_dead_entries() {
        let locks = Arc::new(OwnerLocks::default());
        let a = owner("a");
        let b = owner("b");
        let ga = locks.acquire(&a).await;
        // outro dono não espera
        let gb = tokio::time::timeout(Duration::from_secs(2), locks.acquire(&b))
            .await
            .expect("another owner is not blocked");
        assert_eq!(locks.len(), 2);
        // o mesmo dono espera
        let waiter = {
            let locks = Arc::clone(&locks);
            let a = a.clone();
            tokio::spawn(async move { locks.acquire(&a).await })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "same owner must wait");
        drop(ga);
        let g2 = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("waiter acquires after release")
            .unwrap();
        drop(g2);
        drop(gb);
        assert_eq!(locks.len(), 0, "no dead entries remain");
    }

    #[tokio::test]
    async fn cancelled_lock_waiter_does_not_leak_an_entry_or_the_lock() {
        let locks = Arc::new(OwnerLocks::default());
        let a = owner("a");
        let held = locks.acquire(&a).await;
        let waiter = {
            let locks = Arc::clone(&locks);
            let a = a.clone();
            tokio::spawn(async move {
                let _g = locks.acquire(&a).await;
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        waiter.abort();
        let _ = waiter.await;
        drop(held);
        assert_eq!(locks.len(), 0, "an aborted waiter leaves no entry");
        // e o lock continua utilizável
        drop(locks.acquire(&a).await);
        assert_eq!(locks.len(), 0);
    }

    #[tokio::test]
    async fn many_distinct_owners_leave_the_lock_map_empty() {
        let locks = Arc::new(OwnerLocks::default());
        for i in 0..200 {
            drop(locks.acquire(&owner(&format!("app-{i}"))).await);
        }
        assert_eq!(locks.len(), 0);
    }

    fn quick_service(max_blocking: usize, wait: Duration) -> SqldProvisionService {
        let registry = Arc::new(
            SqldProvisionRegistry::in_memory(Zeroizing::new([5u8; 32]), 64 * 1024 * 1024).unwrap(),
        );
        let vault = Arc::new(AppSecretVault::unavailable());
        let admin =
            SqldAdminClient::new("http://127.0.0.1:1", Zeroizing::new("k".to_owned())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let backup = SqldBackupStore::new("http://127.0.0.1:2", dir.path().to_path_buf());
        // o diretório do backup só precisa existir durante `new`
        let backup = backup.unwrap_or_else(|_| panic!("backup store needs a unix private dir"));
        SqldProvisionService::with_config(
            registry,
            vault,
            admin,
            backup,
            "http://127.0.0.1:3",
            Arc::new(FixedClock(AtomicI64::new(NOW))),
            SqldProvisionConfig {
                max_blocking,
                data_timeout: Duration::from_secs(1),
                admission_wait: wait,
            },
        )
        .unwrap()
    }

    struct FixedClock(AtomicI64);
    impl SqldClock for FixedClock {
        fn now_unix(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn offload_admission_is_held_until_the_job_finishes_even_if_the_await_is_cancelled() {
        let svc = quick_service(1, Duration::from_millis(200));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let finished = Arc::new(AtomicBool::new(false));
        let task = {
            let svc = svc.clone();
            let finished = Arc::clone(&finished);
            tokio::spawn(async move {
                svc.offload(move || {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    finished.store(true, Ordering::SeqCst);
                })
                .await
            })
        };
        // espera o job começar de verdade (fora da thread Tokio)
        tokio::task::spawn_blocking(move || {
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap()
        })
        .await
        .unwrap();
        assert_eq!(svc.inner.admission.available_permits(), 0);
        // cancela o await: o job físico continua e retém o permit
        task.abort();
        let _ = task.await;
        assert!(
            !finished.load(Ordering::SeqCst),
            "job still running after cancel"
        );
        assert_eq!(
            svc.inner.admission.available_permits(),
            0,
            "permit retained by the physical job"
        );
        // uma segunda admissão não passa enquanto o job vive
        assert_eq!(
            svc.offload(|| ()).await.unwrap_err(),
            SqldProvisionError::Busy
        );
        release_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while svc.inner.admission.available_permits() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "permit never returned"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(finished.load(Ordering::SeqCst));
        assert!(svc.offload(|| 7).await.unwrap() == 7);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn offload_runs_off_the_async_thread_and_panics_fail_closed() {
        let svc = quick_service(2, Duration::from_secs(1));
        let caller = std::thread::current().id();
        let worker = svc.offload(|| std::thread::current().id()).await.unwrap();
        assert_ne!(caller, worker, "sync work never runs on the Tokio thread");
        assert_eq!(
            svc.offload(|| -> u8 { panic!("boom") }).await.unwrap_err(),
            SqldProvisionError::Internal
        );
        // o permit do job que paniquou volta
        assert_eq!(svc.inner.admission.available_permits(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn config_and_origin_are_validated() {
        let build = |cfg: SqldProvisionConfig, origin: &str| {
            let registry = Arc::new(
                SqldProvisionRegistry::in_memory(Zeroizing::new([5u8; 32]), 64 * 1024 * 1024)
                    .unwrap(),
            );
            let dir = tempfile::tempdir().unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            SqldProvisionService::with_config(
                registry,
                Arc::new(AppSecretVault::unavailable()),
                SqldAdminClient::new("http://127.0.0.1:1", Zeroizing::new("k".to_owned())).unwrap(),
                SqldBackupStore::new("http://127.0.0.1:2", dir.path().to_path_buf()).unwrap(),
                origin,
                Arc::new(SqldSystemClock),
                cfg,
            )
            .map(|_| ())
        };
        let ok = SqldProvisionConfig::default();
        assert!(build(ok, "http://127.0.0.1:3").is_ok());
        for bad in [
            SqldProvisionConfig {
                max_blocking: 0,
                ..ok
            },
            SqldProvisionConfig {
                data_timeout: Duration::ZERO,
                ..ok
            },
            SqldProvisionConfig {
                data_timeout: Duration::from_secs(16),
                ..ok
            },
            SqldProvisionConfig {
                admission_wait: Duration::ZERO,
                ..ok
            },
        ] {
            assert_eq!(
                build(bad, "http://127.0.0.1:3").unwrap_err(),
                SqldProvisionError::InvalidConfig
            );
        }
        assert_eq!(
            build(ok, "http://127.0.0.1:9090").unwrap_err(),
            SqldProvisionError::InvalidOrigin
        );
    }

    #[test]
    fn debug_and_errors_leak_nothing() {
        let o = SqldOwner::new("secret-worker-name", Some("@secret-ns".into()));
        let text = format!("{o:?}");
        assert!(!text.contains("secret"));
        for err in [
            SqldProvisionError::TokenConflict,
            SqldProvisionError::Registry(SqldRegistryError::Integrity),
            SqldProvisionError::Admin(SqldAdminError::Transport),
            SqldProvisionError::Backup(SqldBackupError::Fs),
        ] {
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains("eyJ") && !shown.contains("http"));
        }
    }
}
