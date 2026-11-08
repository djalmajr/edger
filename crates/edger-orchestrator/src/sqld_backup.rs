//! Escopo EDG-54 / 14b: backup de UM namespace SQLD (`GET /dump`) para um
//! diretório privado do host, com publicação segura e retenção.
//!
//! # Contrato
//!
//! * `GET /dump` na porta de DADOS do SQLD (nunca a API admin: este módulo
//!   não recebe nem conhece credencial admin), com `Authorization: Bearer
//!   <token do app>` e `x-namespace-bin` = `STANDARD_NO_PAD(namespace)` (o
//!   decoder pinado do SQLD 0.24.32 `try_namespace_from_metadata`). A origem
//!   é fixa (validada na construção, mesma fronteira de
//!   `EgressDatabaseGrant`); namespace e token nunca compõem host ou caminho.
//! * Só `200` produz artefato. Redirect, retry e proxy desligados. O
//!   handler pinado (`http/user/dump.rs`) devolve `StreamBody` sem
//!   `Content-Type`. HTTP/1.1 usa framing chunked; erro depois dos headers
//!   chega como erro de corpo (`Incomplete`). Backups por app usam HTTP/1.0
//!   para evitar um frame por fragmento minúsculo: EOF exige também o COMMIT
//!   terminal fora de strings/comentários, antes de autorizar a publicação.
//!   Erros ANTES do stream saem como JSON
//!   com status não-200; um `200` com `Content-Type: application/json`, com
//!   `Content-Encoding` ou sem nenhum byte é recusado (`InvalidResponse`).
//!   A verificação lexical de conclusão é limitada, não valida SQL geral.
//! * Limites: `connect` 5s, leitura 30s, prazo TOTAL 1800s (admissão + corpo
//!   e publicação), cap por app max(256 MiB, 4 x quota do ledger), retenção
//!   fixa de EXATAMENTE 3 cópias por namespace. Um único slot compartilhado
//!   entre clones.
//!
//! # Publicação (Unix)
//!
//! 1. `private_dir` precisa existir, ser diretório (aberto com `O_NOFOLLOW`),
//!    do euid e modo `0700` (checado em `new` e de novo a cada `create`, no
//!    fd aberto). Nada é criado nem `chmod`ado no pai.
//! 2. O primeiro chunk inicia UM job bloqueante (`spawn_blocking`) que é o
//!    dono do slot (o permit MOVE para o job) e do arquivo temporário
//!    `.tmp-<uuid>` (`openat` `O_CREAT|O_EXCL|O_NOFOLLOW`, `fchmod 0600`). Os
//!    chunks chegam por canal limitado; o corpo nunca é coletado em memória.
//!    O cancelamento do chamador solta o canal: o job termina o `write` em voo,
//!    apaga o temporário e só então libera o slot.
//! 3. No fim: `fsync` do arquivo, publicação com `linkat(tmp -> final)` (falha
//!    com `EEXIST` se o final existir: NUNCA sobrescreve), `unlinkat(tmp)` e
//!    `fsync` do diretório. Qualquer falha depois do `linkat` desfaz o final.
//!    O nome final é `<namespace>-<uuid v4 do host>.sql`.
//! 4. Cancelamento/prazo vs publicação: uma máquina de estados atômica
//!    (`Pending -> Committing -> Done`). Cancelar antes de `Committing` impede
//!    o `linkat`; cancelar durante `Committing` faz o job desfazer o final
//!    depois de publicar. Janela residual: um cancelamento que chega DEPOIS de
//!    `Done` (job já publicou, retenção rodando) deixa uma cópia completa e
//!    válida publicada que o chamador não recebeu; a próxima retenção a trata
//!    como cópia comum. Nunca fica cópia parcial.
//! 5. Retenção só depois da cópia segura: considera apenas arquivos com nome
//!    gerado deste namespace, regulares, `0600`, euid, `nlink == 1`, sem
//!    symlink; preserva a nova e as 2 mais recentes (mtime do host, empate por
//!    nome). Outros namespaces, nomes estranhos, symlinks e diretórios nunca
//!    são tocados. Falha de limpeza devolve `RetentionStatus::Incomplete` com
//!    o artefato novo intacto.
//!
//! Não se defende contra outro ator com o mesmo UID ou root dentro do pai
//! `0700`; um crash entre o `linkat` e o `unlinkat` pode deixar um `.tmp-*`
//! (nunca tocado pela retenção). Fora de Unix tudo retorna `Unsupported` antes
//! de qualquer acesso de rede ou arquivo.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use url::Url;
use zeroize::Zeroizing;

/// Cópias retidas por namespace (a nova inclusa). Fixo, não configurável.
pub const KEEP_COPIES: usize = 3;
/// Prazo total padrão e máximo de cada duração configurável.
pub const MAX_CONFIG_TIMEOUT: Duration = Duration::from_secs(1800);
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(1800);
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap legado da API direta e máximo do override explícito de testes.
/// Backups por app usam a quota autenticada e não este máximo fixo.
pub const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024 * 1024;

const NAMESPACE_PREFIX: &str = "edger_";
const NAMESPACE_HEX_LEN: usize = 64;
const NAMESPACE_LEN: usize = NAMESPACE_PREFIX.len() + NAMESPACE_HEX_LEN;
const FINAL_SUFFIX: &str = ".sql";
const TMP_PREFIX: &str = ".tmp-";
/// Chunks em voo entre o stream de rede e o job de escrita.
const CHANNEL_DEPTH: usize = 4;
const BATCH_BYTES: usize = 1024 * 1024;

fn quota_max_bytes(quota_bytes: u64) -> Result<u64, SqldBackupError> {
    if quota_bytes == 0 {
        return Err(SqldBackupError::InvalidConfig);
    }
    quota_bytes
        .checked_mul(4)
        .map(|bytes| bytes.max(DEFAULT_MAX_BYTES))
        .ok_or(SqldBackupError::InvalidConfig)
}

#[derive(Default, Clone, Copy)]
enum SqlLex {
    #[default]
    Normal,
    Quote(u8),
    QuoteEnd(u8),
    Dash,
    Slash,
    LineComment,
    BlockComment,
    BlockStar,
}

/// HTTP/1.0 EOF alone cannot prove completion. SQLD's transaction footer
/// must be a real terminal statement, never text inside a value/comment.
/// This is a bounded lexical completion check, not a general SQL validator.
#[derive(Default)]
struct DumpCompletion {
    state: SqlLex,
    statement: Vec<u8>,
    invalid: bool,
    committed: bool,
}

impl DumpCompletion {
    fn token(&mut self, byte: u8) {
        self.committed = false;
        if self.statement.len() < 32 {
            self.statement.push(byte.to_ascii_uppercase());
        } else {
            self.invalid = true;
        }
    }

    fn push(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            // Most of a large dump is hex BLOB content inside quotes.
            if let SqlLex::Quote(quote) = self.state {
                let Some(at) = bytes.iter().position(|byte| *byte == quote) else {
                    return;
                };
                bytes = &bytes[at + 1..];
                self.state = SqlLex::QuoteEnd(quote);
                continue;
            }
            let byte = bytes[0];
            bytes = &bytes[1..];
            self.byte(byte);
        }
    }

    fn byte(&mut self, byte: u8) {
        match self.state {
            SqlLex::Quote(_) => unreachable!("quotes are scanned by push"),
            SqlLex::QuoteEnd(quote) if byte == quote => self.state = SqlLex::Quote(quote),
            SqlLex::QuoteEnd(_) => {
                self.state = SqlLex::Normal;
                self.byte(byte);
            }
            SqlLex::Dash if byte == b'-' => {
                if self.statement.last().is_some_and(|last| *last != b' ') {
                    self.token(b' ');
                }
                self.state = SqlLex::LineComment;
            }
            SqlLex::Slash if byte == b'*' => {
                if self.statement.last().is_some_and(|last| *last != b' ') {
                    self.token(b' ');
                }
                self.state = SqlLex::BlockComment;
            }
            SqlLex::Dash | SqlLex::Slash => {
                self.token(b'?');
                self.state = SqlLex::Normal;
                self.byte(byte);
            }
            SqlLex::LineComment => {
                if byte == b'\n' {
                    self.state = SqlLex::Normal;
                }
            }
            SqlLex::BlockComment => {
                if byte == b'*' {
                    self.state = SqlLex::BlockStar;
                }
            }
            SqlLex::BlockStar => {
                self.state = match byte {
                    b'/' => SqlLex::Normal,
                    b'*' => SqlLex::BlockStar,
                    _ => SqlLex::BlockComment,
                }
            }
            SqlLex::Normal => match byte {
                b'-' => self.state = SqlLex::Dash,
                b'/' => self.state = SqlLex::Slash,
                b'\'' | b'"' | b'`' | b'[' => {
                    self.token(b'?');
                    self.invalid = true;
                    self.state = SqlLex::Quote(if byte == b'[' { b']' } else { byte });
                }
                b';' => {
                    self.committed = !self.invalid
                        && matches!(
                            self.statement.trim_ascii(),
                            b"COMMIT" | b"COMMIT TRANSACTION"
                        );
                    self.statement.clear();
                    self.invalid = false;
                }
                byte if byte.is_ascii_whitespace() => {
                    if self.statement.last().is_some_and(|last| *last != b' ') {
                        self.token(b' ');
                    }
                }
                byte => self.token(byte),
            },
        }
    }

    fn complete(&self) -> bool {
        matches!(self.state, SqlLex::Normal) && self.committed && self.statement.is_empty()
    }
}

/// Erros estáticos: nunca carregam token, corpo, query nem resposta SQLD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldBackupError {
    /// Plataforma sem as primitivas de arquivo exigidas (fora de Unix).
    Unsupported,
    InvalidOrigin,
    InvalidConfig,
    InvalidDirectory,
    InvalidNamespace,
    InvalidToken,
    Client,
    /// O slot não ficou livre dentro do prazo total.
    AdmissionTimeout,
    Timeout,
    Transport,
    Redirect,
    Unauthorized,
    UnexpectedStatus,
    /// `200` com forma que não é um dump (JSON, encoding, vazio).
    InvalidResponse,
    TooLarge,
    /// O corpo terminou antes do fim declarado pelo protocolo.
    Incomplete,
    Fs,
    /// O job foi cancelado antes de publicar.
    Cancelled,
}

impl fmt::Display for SqldBackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "sqld backup: unsupported platform",
            Self::InvalidOrigin => "sqld backup: invalid origin",
            Self::InvalidConfig => "sqld backup: invalid config",
            Self::InvalidDirectory => "sqld backup: invalid private directory",
            Self::InvalidNamespace => "sqld backup: invalid namespace",
            Self::InvalidToken => "sqld backup: invalid token",
            Self::Client => "sqld backup: cannot build http client",
            Self::AdmissionTimeout => "sqld backup: admission deadline exceeded",
            Self::Timeout => "sqld backup: deadline exceeded",
            Self::Transport => "sqld backup: transport error",
            Self::Redirect => "sqld backup: unexpected redirect",
            Self::Unauthorized => "sqld backup: unauthorized",
            Self::UnexpectedStatus => "sqld backup: unexpected status",
            Self::InvalidResponse => "sqld backup: invalid response",
            Self::TooLarge => "sqld backup: dump too large",
            Self::Incomplete => "sqld backup: incomplete dump body",
            Self::Fs => "sqld backup: file system error",
            Self::Cancelled => "sqld backup: cancelled",
        })
    }
}

impl std::error::Error for SqldBackupError {}

/// Prazos e cap. Tudo positivo e limitado; a retenção não é configurável.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqldBackupConfig {
    pub total_timeout: Duration,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    pub max_bytes: u64,
}

impl Default for SqldBackupConfig {
    fn default() -> Self {
        Self {
            total_timeout: DEFAULT_TOTAL_TIMEOUT,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl SqldBackupConfig {
    fn validate(&self) -> Result<(), SqldBackupError> {
        let bad_duration = |d: Duration| d.is_zero() || d > MAX_CONFIG_TIMEOUT;
        if bad_duration(self.total_timeout)
            || bad_duration(self.connect_timeout)
            || bad_duration(self.read_timeout)
            || self.max_bytes == 0
            || self.max_bytes > MAX_CONFIG_BYTES
        {
            return Err(SqldBackupError::InvalidConfig);
        }
        Ok(())
    }
}

/// Estado da retenção depois da publicação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionStatus {
    Complete,
    /// A cópia nova e as anteriores estão intactas; alguma limpeza falhou.
    Incomplete,
}

/// Metadado de uma cópia publicada: só o nome final (sem caminho).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupArtifact {
    pub file_name: String,
    pub bytes: u64,
    /// SHA-256 hexadecimal minúsculo dos bytes gravados.
    pub sha256: String,
    pub retention: RetentionStatus,
}

#[cfg_attr(not(unix), allow(dead_code))]
struct Inner {
    client: Client,
    dump_url: Url,
    dir: PathBuf,
    slot: Arc<Semaphore>,
    total_timeout: Duration,
    max_bytes: u64,
    #[cfg(all(test, unix))]
    write_gate: std::sync::Mutex<Option<tests::WriteGate>>,
    #[cfg(all(test, unix))]
    writes: Arc<std::sync::atomic::AtomicUsize>,
}

/// Serviço de backup; clones compartilham o mesmo slot.
#[derive(Clone)]
pub struct SqldBackupStore {
    inner: Arc<Inner>,
}

impl fmt::Debug for SqldBackupStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldBackupStore").finish_non_exhaustive()
    }
}

impl SqldBackupStore {
    pub fn new(data_origin: &str, private_dir: PathBuf) -> Result<Self, SqldBackupError> {
        Self::with_config(data_origin, private_dir, SqldBackupConfig::default())
    }

    pub fn with_config(
        data_origin: &str,
        private_dir: PathBuf,
        config: SqldBackupConfig,
    ) -> Result<Self, SqldBackupError> {
        #[cfg(not(unix))]
        {
            let _ = (data_origin, private_dir, config);
            Err(SqldBackupError::Unsupported)
        }
        #[cfg(unix)]
        {
            config.validate()?;
            let mut dump_url = parse_data_origin(data_origin)?;
            dump_url.set_path("/dump");
            if !private_dir.is_absolute() {
                return Err(SqldBackupError::InvalidDirectory);
            }
            drop(unix_fs::open_private_dir(&private_dir)?);
            let client = Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(config.connect_timeout)
                .read_timeout(config.read_timeout)
                .build()
                .map_err(|_| SqldBackupError::Client)?;
            Ok(Self {
                inner: Arc::new(Inner {
                    client,
                    dump_url,
                    dir: private_dir,
                    slot: Arc::new(Semaphore::new(1)),
                    total_timeout: config.total_timeout,
                    max_bytes: config.max_bytes,
                    #[cfg(all(test, unix))]
                    write_gate: std::sync::Mutex::new(None),
                    #[cfg(all(test, unix))]
                    writes: Arc::default(),
                }),
            })
        }
    }

    /// Faz `GET /dump` do namespace e publica a cópia. O token é consumido e
    /// zeroizado ao fim; nenhum erro ou `Debug` o contém.
    pub async fn create(
        &self,
        namespace: &str,
        token: Zeroizing<String>,
    ) -> Result<BackupArtifact, SqldBackupError> {
        self.create_with_limit(
            namespace,
            token,
            self.inner.max_bytes,
            reqwest::Version::HTTP_11,
        )
        .await
    }

    /// App backups use the authenticated ledger quota, never the dump's headers.
    /// A non-default explicit cap must cover the derived minimum or fail visibly.
    pub async fn create_for_quota(
        &self,
        namespace: &str,
        token: Zeroizing<String>,
        quota_bytes: u64,
    ) -> Result<BackupArtifact, SqldBackupError> {
        let required = quota_max_bytes(quota_bytes)?;
        let configured = self.inner.max_bytes;
        if configured != DEFAULT_MAX_BYTES && configured < required {
            return Err(SqldBackupError::InvalidConfig);
        }
        self.create_with_limit(
            namespace,
            token,
            required.max(configured),
            reqwest::Version::HTTP_10,
        )
        .await
    }

    async fn create_with_limit(
        &self,
        namespace: &str,
        token: Zeroizing<String>,
        max_bytes: u64,
        version: reqwest::Version,
    ) -> Result<BackupArtifact, SqldBackupError> {
        #[cfg(not(unix))]
        {
            let _ = (namespace, token, max_bytes, version);
            Err(SqldBackupError::Unsupported)
        }
        #[cfg(unix)]
        {
            validate_namespace(namespace)?;
            let authorization = authorization_header(&token)?;
            drop(token);
            let namespace_header = namespace_header(namespace);
            // Prazo absoluto único: nunca reiniciado.
            let deadline = Instant::now()
                .checked_add(self.inner.total_timeout)
                .ok_or(SqldBackupError::InvalidConfig)?;
            let permit =
                tokio::time::timeout_at(deadline, Arc::clone(&self.inner.slot).acquire_owned())
                    .await
                    .map_err(|_| SqldBackupError::AdmissionTimeout)?
                    .map_err(|_| SqldBackupError::Transport)?;
            tokio::time::timeout_at(
                deadline,
                self.drive(
                    namespace,
                    authorization,
                    namespace_header,
                    permit,
                    deadline,
                    max_bytes,
                    version,
                ),
            )
            .await
            .map_err(|_| SqldBackupError::Timeout)?
        }
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    async fn drive(
        &self,
        namespace: &str,
        authorization: HeaderValue,
        namespace_header: HeaderValue,
        permit: tokio::sync::OwnedSemaphorePermit,
        deadline: Instant,
        max_bytes: u64,
        version: reqwest::Version,
    ) -> Result<BackupArtifact, SqldBackupError> {
        let inner = &self.inner;
        let mut response = inner
            .client
            .get(inner.dump_url.clone())
            // SQLD emits tiny pieces of hex BLOBs. HTTP/1.0 makes an
            // unknown-length dump EOF-delimited instead of framing every
            // piece as its own HTTP chunk/body-channel message.
            .version(version)
            .header(AUTHORIZATION, authorization)
            .header("x-namespace-bin", namespace_header)
            .send()
            .await
            .map_err(map_send_error)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(SqldBackupError::Redirect);
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(SqldBackupError::Unauthorized);
        }
        if status != StatusCode::OK {
            return Err(SqldBackupError::UnexpectedStatus);
        }
        if response.headers().contains_key(CONTENT_ENCODING) {
            return Err(SqldBackupError::InvalidResponse);
        }
        if response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(is_json_content_type)
        {
            return Err(SqldBackupError::InvalidResponse);
        }
        if response.content_length().is_some_and(|len| len > max_bytes) {
            return Err(SqldBackupError::TooLarge);
        }

        #[cfg(not(test))]
        let control = Arc::new(unix_fs::Control::default());
        #[cfg(test)]
        let control = Arc::new(unix_fs::Control::with_writes(Arc::clone(&inner.writes)));
        let mut guard = unix_fs::CancelGuard::new(Arc::clone(&control));
        let mut writer: Option<unix_fs::JobHandle> = None;
        let pumped = self
            .pump(
                &mut response,
                namespace,
                permit,
                &control,
                deadline,
                &mut writer,
                max_bytes,
                version == reqwest::Version::HTTP_10,
            )
            .await;
        match pumped {
            Ok(()) => {
                let mut handle = writer.ok_or(SqldBackupError::InvalidResponse)?;
                let result = handle.finish().await;
                guard.disarm();
                // Resultado pronto não vale depois do prazo só porque o
                // runtime ainda não observou o timer. Se a cópia já foi
                // concluída (retenção incluída) a cópia COMPLETA fica
                // publicada e nenhum backup antigo é desfeito.
                match result {
                    Ok(_) if Instant::now() >= deadline => Err(SqldBackupError::Timeout),
                    other => other,
                }
            }
            Err(err) => {
                // Falha antes do `Finish`: solta o canal e ESPERA o job
                // apagar o temporário e liberar o slot, para que o retorno
                // já mostre o diretório sem resíduo.
                if let Some(handle) = writer {
                    handle.abandon().await;
                }
                guard.disarm();
                Err(err)
            }
        }
    }

    /// Corpo -> job de escrita. `Ok` só depois de enviar `Finish`.
    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    async fn pump(
        &self,
        response: &mut reqwest::Response,
        namespace: &str,
        permit: tokio::sync::OwnedSemaphorePermit,
        control: &Arc<unix_fs::Control>,
        deadline: Instant,
        writer: &mut Option<unix_fs::JobHandle>,
        max_bytes: u64,
        validate_sql: bool,
    ) -> Result<(), SqldBackupError> {
        let inner = &self.inner;
        let mut permit = Some(permit);
        let mut total: u64 = 0;
        let mut batch = bytes::BytesMut::with_capacity(BATCH_BYTES);
        let mut first = true;
        let mut completion = DumpCompletion::default();
        loop {
            let mut chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(err) if err.is_timeout() => return Err(SqldBackupError::Timeout),
                Err(_) => return Err(SqldBackupError::Incomplete),
            };
            if chunk.is_empty() {
                continue;
            }
            // Soma checada ANTES de entregar o chunk ao writer.
            total = match total.checked_add(chunk.len() as u64) {
                Some(sum) if sum <= max_bytes => sum,
                _ => return Err(SqldBackupError::TooLarge),
            };
            if validate_sql {
                completion.push(&chunk);
            }
            if writer.is_none() {
                let permit = permit.take().ok_or(SqldBackupError::Transport)?;
                *writer = Some(unix_fs::spawn_job(unix_fs::JobArgs {
                    permit,
                    dir: inner.dir.clone(),
                    namespace: namespace.to_owned(),
                    control: Arc::clone(control),
                    deadline: deadline.into_std(),
                    #[cfg(all(test, unix))]
                    gate: inner.write_gate.lock().ok().and_then(|mut g| g.take()),
                }));
            }
            let handle = writer.as_mut().ok_or(SqldBackupError::Transport)?;
            // Keep the first write prompt (including cancellation ownership),
            // then coalesce the tiny SQLD fragments into byte-bounded messages.
            if first {
                let head =
                    bytes::Bytes::copy_from_slice(&chunk.split_to(chunk.len().min(BATCH_BYTES)));
                handle.send(unix_fs::Msg::Chunk(head)).await?;
                first = false;
            }
            while !chunk.is_empty() {
                let count = chunk.len().min(BATCH_BYTES - batch.len());
                batch.extend_from_slice(&chunk.split_to(count));
                if batch.len() == BATCH_BYTES {
                    handle
                        .send(unix_fs::Msg::Chunk(batch.split().freeze()))
                        .await?;
                }
            }
        }
        // Corpo vazio nunca é um dump (e nunca roda fs).
        let handle = writer.as_mut().ok_or(SqldBackupError::InvalidResponse)?;
        if validate_sql && !completion.complete() {
            return Err(SqldBackupError::Incomplete);
        }
        if !batch.is_empty() {
            handle.send(unix_fs::Msg::Chunk(batch.freeze())).await?;
        }
        handle.send(unix_fs::Msg::Finish(total)).await
    }
}

fn is_json_content_type(value: &str) -> bool {
    let essence = value.split(';').next().unwrap_or("").trim();
    essence.eq_ignore_ascii_case("application/json")
        || essence
            .rsplit_once('+')
            .is_some_and(|(_, s)| s.eq_ignore_ascii_case("json"))
}

fn map_send_error(err: reqwest::Error) -> SqldBackupError {
    if err.is_timeout() {
        SqldBackupError::Timeout
    } else {
        SqldBackupError::Transport
    }
}

/// Mesma fronteira de `EgressDatabaseGrant`: `http`, host, sem userinfo, sem
/// query/fragment, caminho `/`, porta fixa diferente de 9090.
fn parse_data_origin(origin: &str) -> Result<Url, SqldBackupError> {
    let invalid = SqldBackupError::InvalidOrigin;
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

/// `edger_` + 64 hex minúsculos.
fn validate_namespace(namespace: &str) -> Result<(), SqldBackupError> {
    let ok = namespace.strip_prefix(NAMESPACE_PREFIX).is_some_and(|hex| {
        hex.len() == NAMESPACE_HEX_LEN
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if ok {
        Ok(())
    } else {
        Err(SqldBackupError::InvalidNamespace)
    }
}

fn namespace_header(namespace: &str) -> HeaderValue {
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(namespace.as_bytes());
    // NO_PAD base64 é `[A-Za-z0-9+/]`: sempre um header válido.
    HeaderValue::from_str(&encoded).expect("NO_PAD base64 is a valid header value")
}

/// Token não vazio, ASCII visível sem espaço; header marcado sensível.
fn authorization_header(token: &Zeroizing<String>) -> Result<HeaderValue, SqldBackupError> {
    if token.is_empty() || !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(SqldBackupError::InvalidToken);
    }
    let line = Zeroizing::new(format!("Bearer {}", token.as_str()));
    let mut value =
        HeaderValue::from_str(line.as_str()).map_err(|_| SqldBackupError::InvalidToken)?;
    value.set_sensitive(true);
    Ok(value)
}

/// `<namespace>-<uuid canônico minúsculo>.sql`; `None` para qualquer outra
/// forma (nunca é um nome gerado por este módulo).
#[cfg_attr(not(unix), allow(dead_code))]
fn parse_backup_name(name: &str, namespace: &str) -> Option<()> {
    let stem = name.strip_suffix(FINAL_SUFFIX)?;
    let (ns, rest) = (stem.get(..NAMESPACE_LEN)?, stem.get(NAMESPACE_LEN..)?);
    let uuid = rest.strip_prefix('-')?;
    if ns != namespace {
        return None;
    }
    let parsed = uuid::Uuid::parse_str(uuid).ok()?;
    (parsed.hyphenated().to_string() == uuid).then_some(())
}

#[cfg(unix)]
mod unix_fs {
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::{AsFd, OwnedFd};
    use std::path::Path;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Arc;

    use bytes::Bytes;
    use rustix::fs::{self, AtFlags, Dir, FileType, Mode, OFlags, Stat, CWD};
    use rustix::io::Errno;
    use sha2::{Digest, Sha256};
    use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit};

    use super::{
        parse_backup_name, BackupArtifact, RetentionStatus, SqldBackupError, CHANNEL_DEPTH,
        FINAL_SUFFIX, KEEP_COPIES, TMP_PREFIX,
    };

    /// Só testes: força a falha da limpeza (o `unlinkat` real não falha de
    /// forma portável num diretório 0700 do próprio euid).
    #[cfg(all(test, unix))]
    pub(super) static FORCE_RETENTION_UNLINK_FAILURE: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    const PENDING: u8 = 0;
    const COMMITTING: u8 = 1;
    const DONE: u8 = 2;
    const CANCELLED: u8 = 3;
    const CANCELLED_AFTER_COMMIT: u8 = 4;

    /// Estado atômico de publicação compartilhado entre o chamador e o job.
    #[derive(Default)]
    pub(super) struct Control {
        state: AtomicU8,
        #[cfg(test)]
        writes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Control {
        #[cfg(test)]
        pub(super) fn with_writes(writes: Arc<std::sync::atomic::AtomicUsize>) -> Self {
            Self {
                writes,
                ..Self::default()
            }
        }
        /// Chamador: pede cancelamento. Antes do commit impede a publicação;
        /// durante o commit o job desfaz o final depois de publicar.
        pub(super) fn request_cancel(&self) {
            if self
                .state
                .compare_exchange(PENDING, CANCELLED, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                let _ = self.state.compare_exchange(
                    COMMITTING,
                    CANCELLED_AFTER_COMMIT,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
            }
        }

        fn begin_commit(&self) -> bool {
            self.state
                .compare_exchange(PENDING, COMMITTING, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        }

        fn finish_commit(&self) -> bool {
            self.state
                .compare_exchange(COMMITTING, DONE, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        }
    }

    /// Cancela o job se o chamador sair (drop/timeout) sem receber o resultado.
    pub(super) struct CancelGuard {
        control: Option<Arc<Control>>,
    }

    impl CancelGuard {
        pub(super) fn new(control: Arc<Control>) -> Self {
            Self {
                control: Some(control),
            }
        }

        pub(super) fn disarm(&mut self) {
            self.control = None;
        }
    }

    impl Drop for CancelGuard {
        fn drop(&mut self) {
            if let Some(control) = self.control.take() {
                control.request_cancel();
            }
        }
    }

    pub(super) enum Msg {
        Chunk(Bytes),
        /// Total de bytes que o chamador entregou (conferido pelo job).
        Finish(u64),
    }

    pub(super) struct JobHandle {
        tx: Option<mpsc::Sender<Msg>>,
        done: Option<oneshot::Receiver<Result<BackupArtifact, SqldBackupError>>>,
    }

    impl JobHandle {
        /// Entrega uma mensagem; se o canal fechou o job já falhou e o erro
        /// real dele é devolvido.
        pub(super) async fn send(&mut self, msg: Msg) -> Result<(), SqldBackupError> {
            let Some(tx) = self.tx.as_ref() else {
                return Err(SqldBackupError::Fs);
            };
            if tx.send(msg).await.is_ok() {
                return Ok(());
            }
            self.tx = None;
            Err(match self.done.take() {
                Some(done) => match done.await {
                    Ok(Err(err)) => err,
                    _ => SqldBackupError::Fs,
                },
                None => SqldBackupError::Fs,
            })
        }

        /// Depois do `Finish`: o resultado do job.
        pub(super) async fn finish(&mut self) -> Result<BackupArtifact, SqldBackupError> {
            self.tx = None;
            match self.done.take() {
                Some(done) => done.await.unwrap_or(Err(SqldBackupError::Fs)),
                None => Err(SqldBackupError::Fs),
            }
        }

        /// Solta o canal sem `Finish` e espera o job terminar de verdade.
        pub(super) async fn abandon(mut self) {
            self.tx = None;
            if let Some(done) = self.done.take() {
                let _ = done.await;
            }
        }
    }

    pub(super) struct JobArgs {
        pub(super) permit: OwnedSemaphorePermit,
        pub(super) dir: std::path::PathBuf,
        pub(super) namespace: String,
        pub(super) control: Arc<Control>,
        pub(super) deadline: std::time::Instant,
        #[cfg(all(test, unix))]
        pub(super) gate: Option<super::tests::WriteGate>,
    }

    /// Job bloqueante dono do slot e do temporário até o fim real.
    pub(super) fn spawn_job(args: JobArgs) -> JobHandle {
        let (tx, rx) = mpsc::channel(CHANNEL_DEPTH);
        let (done_tx, done) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            // O permit só sai com o job: cancelar o chamador não o libera.
            let JobArgs {
                permit,
                dir,
                namespace,
                control,
                deadline,
                #[cfg(all(test, unix))]
                gate,
            } = args;
            let _permit = permit;
            let result = run_job(
                &dir,
                &namespace,
                &control,
                deadline,
                rx,
                #[cfg(all(test, unix))]
                gate,
                #[cfg(all(test, unix))]
                None,
            );
            let _ = done_tx.send(result);
        });
        JobHandle {
            tx: Some(tx),
            done: Some(done),
        }
    }

    #[allow(clippy::unnecessary_cast)]
    fn mode_bits(st: &Stat) -> u32 {
        st.st_mode as u32
    }

    /// Diretório existente, aberto sem seguir symlink, do euid e `0700`.
    pub(super) fn open_private_dir(path: &Path) -> Result<OwnedFd, SqldBackupError> {
        let invalid = SqldBackupError::InvalidDirectory;
        let dir = fs::openat(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| invalid)?;
        let st = fs::fstat(&dir).map_err(|_| invalid)?;
        if st.st_uid != rustix::process::geteuid().as_raw() || mode_bits(&st) & 0o7777 != 0o700 {
            return Err(invalid);
        }
        Ok(dir)
    }

    /// Temporário privado com remoção garantida (a menos que publicado).
    struct Staging<'a> {
        dir: &'a OwnedFd,
        name: String,
        armed: bool,
    }

    impl Drop for Staging<'_> {
        fn drop(&mut self) {
            if self.armed {
                let _ = fs::unlinkat(self.dir, self.name.as_str(), AtFlags::empty());
            }
        }
    }

    pub(super) fn run_job(
        dir_path: &Path,
        namespace: &str,
        control: &Control,
        deadline: std::time::Instant,
        mut rx: mpsc::Receiver<Msg>,
        #[cfg(all(test, unix))] mut gate: Option<super::tests::WriteGate>,
        #[cfg(all(test, unix))] after_link: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<BackupArtifact, SqldBackupError> {
        let fs_err = |_| SqldBackupError::Fs;
        let dir = open_private_dir(dir_path)?;
        let uuid = uuid::Uuid::new_v4().hyphenated().to_string();
        let final_name = format!("{namespace}-{uuid}{FINAL_SUFFIX}");
        let tmp_name = format!("{TMP_PREFIX}{uuid}");
        let fd = fs::openat(
            &dir,
            tmp_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(fs_err)?;
        let mut staging = Staging {
            dir: &dir,
            name: tmp_name,
            armed: true,
        };
        // O umask pode ter reduzido o modo; o arquivo é nosso.
        fs::fchmod(&fd, Mode::from_bits_truncate(0o600)).map_err(fs_err)?;
        let mut file = File::from(fd);
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        let expected = loop {
            match rx.blocking_recv() {
                Some(Msg::Chunk(chunk)) => {
                    #[cfg(all(test, unix))]
                    if let Some(gate) = gate.take() {
                        gate.pass();
                    }
                    file.write_all(&chunk).map_err(|_| SqldBackupError::Fs)?;
                    #[cfg(test)]
                    control.writes.fetch_add(1, Ordering::SeqCst);
                    hasher.update(&chunk);
                    written = written
                        .checked_add(chunk.len() as u64)
                        .ok_or(SqldBackupError::TooLarge)?;
                }
                Some(Msg::Finish(total)) => break total,
                // Canal solto sem `Finish`: cancelamento/erro do chamador.
                None => return Err(SqldBackupError::Cancelled),
            }
        };
        if written != expected || written == 0 {
            return Err(SqldBackupError::Incomplete);
        }
        file.sync_all().map_err(|_| SqldBackupError::Fs)?;
        drop(file);
        if std::time::Instant::now() >= deadline {
            return Err(SqldBackupError::Timeout);
        }
        if !control.begin_commit() {
            return Err(SqldBackupError::Cancelled);
        }
        // `linkat` NUNCA sobrescreve: `EEXIST` se o final existir.
        fs::linkat(
            &dir,
            staging.name.as_str(),
            &dir,
            final_name.as_str(),
            AtFlags::empty(),
        )
        .map_err(fs_err)?;
        let rollback = |dir: &OwnedFd| {
            let _ = fs::unlinkat(dir, final_name.as_str(), AtFlags::empty());
            let _ = fs::fsync(dir);
        };
        // O temporário sai: o final volta a ter `nlink == 1`.
        if fs::unlinkat(&dir, staging.name.as_str(), AtFlags::empty()).is_err() {
            rollback(&dir);
            return Err(SqldBackupError::Fs);
        }
        staging.armed = false;
        #[cfg(all(test, unix))]
        if let Some(hook) = after_link {
            hook();
        }
        if fs::fsync(&dir).is_err() {
            rollback(&dir);
            return Err(SqldBackupError::Fs);
        }
        // Prazo absoluto rechecado DEPOIS de linkat/unlink/fsync e ANTES de
        // `Done`: o job bloqueante não depende do cancelamento assíncrono
        // (ainda não entregue) para recusar um sucesso tardio. Vencido:
        // desfaz o final, sem `Done` e sem retenção.
        if std::time::Instant::now() >= deadline {
            rollback(&dir);
            return Err(SqldBackupError::Timeout);
        }
        if !control.finish_commit() {
            // Cancelado durante a janela de publicação: desfaz o final.
            rollback(&dir);
            return Err(SqldBackupError::Cancelled);
        }
        let retention = retain(&dir, namespace, &final_name);
        Ok(BackupArtifact {
            file_name: final_name,
            bytes: written,
            sha256: hex(&hasher.finalize()),
            retention,
        })
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    #[allow(clippy::unnecessary_cast)]
    fn mtime_key(st: &Stat) -> (i64, i64) {
        (st.st_mtime as i64, st.st_mtime_nsec as i64)
    }

    /// Retenção: preserva `keep` (a nova) e as `KEEP_COPIES - 1` anteriores
    /// mais recentes entre os arquivos elegíveis DESTE namespace.
    fn retain(dir: &OwnedFd, namespace: &str, keep: &str) -> RetentionStatus {
        let euid = rustix::process::geteuid().as_raw();
        let Ok(mut entries) = Dir::read_from(dir.as_fd()) else {
            return RetentionStatus::Incomplete;
        };
        let mut incomplete = false;
        let mut candidates: Vec<((i64, i64), String)> = Vec::new();
        while let Some(entry) = entries.read() {
            let Ok(entry) = entry else {
                incomplete = true;
                break;
            };
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if name == keep || parse_backup_name(name, namespace).is_none() {
                continue;
            }
            let Ok(st) = fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) else {
                continue;
            };
            #[allow(clippy::unnecessary_cast)]
            let eligible = FileType::from_raw_mode(st.st_mode) == FileType::RegularFile
                && st.st_uid == euid
                && mode_bits(&st) & 0o7777 == 0o600
                && st.st_nlink as u64 == 1;
            if eligible {
                candidates.push((mtime_key(&st), name.to_owned()));
            }
        }
        // Mais recentes primeiro; empate por nome (estável, não por readdir).
        candidates.sort_by(|a, b| b.cmp(a));
        let mut removed = false;
        for (_, name) in candidates.into_iter().skip(KEEP_COPIES - 1) {
            #[cfg(all(test, unix))]
            if FORCE_RETENTION_UNLINK_FAILURE.load(Ordering::SeqCst) {
                incomplete = true;
                continue;
            }
            match fs::unlinkat(dir, name.as_str(), AtFlags::empty()) {
                Ok(()) => removed = true,
                Err(Errno::NOENT) => {}
                Err(_) => incomplete = true,
            }
        }
        if removed && fs::fsync(dir).is_err() {
            incomplete = true;
        }
        if incomplete {
            RetentionStatus::Incomplete
        } else {
            RetentionStatus::Complete
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc as std_mpsc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn edg87_quota_admits_the_serialized_one_gib_dump() {
        assert_eq!(quota_max_bytes(0), Err(SqldBackupError::InvalidConfig));
        assert_eq!(
            quota_max_bytes(u64::MAX),
            Err(SqldBackupError::InvalidConfig)
        );
        assert_eq!(
            quota_max_bytes(1024 * 1024 * 1024).unwrap(),
            4 * 1024 * 1024 * 1024
        );
        assert!(quota_max_bytes(1024 * 1024 * 1024).unwrap() > 2_147_513_437);
        assert_eq!(quota_max_bytes(4096).unwrap(), DEFAULT_MAX_BYTES);
    }

    #[test]
    fn edg87_completion_ignores_quoted_and_commented_fake_footers_across_chunks() {
        let good = b"PRAGMA foreign_keys=OFF;\nBEGIN;\nCREATE TABLE \"a;\"(v);\nINSERT INTO \"a;\" VALUES ('a'';\nCOMMIT;\n');\nCOMMIT;\n";
        for size in [1, 2, 7, good.len()] {
            let mut completion = DumpCompletion::default();
            for chunk in good.chunks(size) {
                completion.push(chunk);
            }
            assert!(completion.complete(), "chunk size {size}");
        }
        for bad in [
            b"BEGIN;\nINSERT INTO t VALUES ('payload\nCOMMIT;\n".as_slice(),
            b"BEGIN;\n-- COMMIT;\n",
            b"BEGIN;\n/* COMMIT; */\n",
            b"BEGIN;\nINSERT INTO t VALUES ('x'';COMMIT;');\n",
            b"BEGIN;\nINSERT INTO [COMMIT;] VALUES (1);\n",
            b"BEGIN;\nINSERT INTO `COMMIT;` VALUES (1);\n",
            b"BEGIN;\nCOM/* not one token */MIT;\n",
            b"BEGIN;\nCOM-- not one token\nMIT;\n",
            b"BEGIN;\nCOMMIT;\nINSERT INTO t VALUES ('unfinished",
        ] {
            let mut completion = DumpCompletion::default();
            for byte in bad.chunks(1) {
                completion.push(byte);
            }
            assert!(!completion.complete(), "fake completion: {bad:?}");
        }
    }

    #[tokio::test]
    async fn edg87_tiny_chunks_use_few_real_writes_with_identical_bytes_and_hash() {
        use sha2::{Digest, Sha256};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let serve = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            for _ in 0..4096 {
                socket.write_all(b"2\r\nab\r\n").await.unwrap();
            }
            socket.write_all(b"0\r\n\r\n").await.unwrap();
        });
        let dir = private_dir();
        let store = SqldBackupStore::new(&origin, dir.path().to_path_buf()).unwrap();
        let artifact = store
            .create(NS, Zeroizing::new("test".into()))
            .await
            .unwrap();
        serve.await.unwrap();
        let expected = b"ab".repeat(4096);
        assert_eq!(
            std::fs::read(dir.path().join(artifact.file_name)).unwrap(),
            expected
        );
        assert_eq!(artifact.bytes, expected.len() as u64);
        assert_eq!(artifact.sha256, format!("{:x}", Sha256::digest(&expected)));
        let writes = store.inner.writes.load(std::sync::atomic::Ordering::SeqCst);
        assert!(writes <= 2, "4096 tiny chunks caused {writes} real writes");
    }

    /// Trava o job dentro do primeiro `write` até o teste liberar.
    pub(crate) struct WriteGate {
        entered: std_mpsc::Sender<()>,
        release: std_mpsc::Receiver<()>,
    }

    impl WriteGate {
        pub(crate) fn pass(self) {
            let _ = self.entered.send(());
            let _ = self.release.recv();
        }
    }

    const NS: &str = "edger_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Servidor TCP cru: responde chunked, devolve o primeiro chunk e depois
    /// SEGURA a conexão aberta até o teste soltar `hold`.
    async fn stalled_server() -> (
        String,
        tokio::sync::oneshot::Sender<()>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&accepted);
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
            sock.write_all(head).await.unwrap();
            sock.write_all(b"5\r\nhello\r\n").await.unwrap();
            let _ = hold_rx.await;
        });
        (origin, hold_tx, accepted)
    }

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    /// Cancelar o chamador NÃO libera o slot enquanto o `write` bloqueante
    /// que ele iniciou ainda executa; o temporário só sai com o job.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_keeps_the_slot_until_the_blocking_write_returns() {
        let (origin, hold, accepted) = stalled_server().await;
        let dir = private_dir();
        let store = SqldBackupStore::with_config(
            &origin,
            dir.path().to_path_buf(),
            SqldBackupConfig {
                total_timeout: Duration::from_secs(2),
                read_timeout: Duration::from_secs(30),
                ..SqldBackupConfig::default()
            },
        )
        .unwrap();
        let (entered_tx, entered_rx) = std_mpsc::channel();
        let (release_tx, release_rx) = std_mpsc::channel();
        *store.inner.write_gate.lock().unwrap() = Some(WriteGate {
            entered: entered_tx,
            release: release_rx,
        });

        let first = {
            let store = store.clone();
            tokio::spawn(async move { store.create(NS, Zeroizing::new("tok".to_owned())).await })
        };
        // O job está DENTRO do write bloqueante.
        tokio::task::spawn_blocking(move || {
            entered_rx.recv_timeout(Duration::from_secs(10)).unwrap()
        })
        .await
        .unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());

        // Chamador cancelado, job ainda no write: slot continua ocupado.
        assert_eq!(store.inner.slot.available_permits(), 0);
        // Admissão: o segundo chamador estoura o prazo SEM nenhum pedido.
        let second = store.create(NS, Zeroizing::new("tok".to_owned())).await;
        assert_eq!(second.unwrap_err(), SqldBackupError::AdmissionTimeout);
        assert_eq!(accepted.load(std::sync::atomic::Ordering::SeqCst), 1);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 1, "only the staging file exists: {names:?}");
        assert!(names[0].starts_with(TMP_PREFIX));

        // O write retorna: o job vê o canal solto, apaga o temporário e só
        // então libera o slot.
        release_tx.send(()).unwrap();
        let permit = tokio::time::timeout(
            Duration::from_secs(10),
            Arc::clone(&store.inner.slot).acquire_owned(),
        )
        .await
        .expect("slot is released after the job ends")
        .unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        drop(permit);
        let _ = hold.send(());
    }

    async fn ok_server(body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                sock.write_all(head.as_bytes()).await.unwrap();
                sock.write_all(body).await.unwrap();
                let _ = sock.shutdown().await;
            }
        });
        origin
    }

    fn seed(dir: &std::path::Path, n: u128, mtime: i64) -> String {
        let name = format!("{NS}-{}.sql", uuid::Uuid::from_u128(n).hyphenated());
        let path = dir.join(&name);
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let ts = rustix::fs::Timespec {
            tv_sec: mtime,
            tv_nsec: 0,
        };
        rustix::fs::utimensat(
            rustix::fs::CWD,
            &path,
            &rustix::fs::Timestamps {
                last_access: ts,
                last_modification: ts,
            },
            rustix::fs::AtFlags::empty(),
        )
        .unwrap();
        name
    }

    /// Falha de limpeza: a cópia nova e as anteriores ficam intactas e o
    /// artefato diz `Incomplete` (não declara perda da cópia publicada).
    #[tokio::test]
    async fn retention_failure_reports_incomplete_and_keeps_everything() {
        let origin = ok_server(b"dump").await;
        let dir = private_dir();
        let old: Vec<String> = (1..=4)
            .map(|n| seed(dir.path(), n, 1_000 + n as i64))
            .collect();
        let store = SqldBackupStore::new(&origin, dir.path().to_path_buf()).unwrap();
        let flag = &unix_fs::FORCE_RETENTION_UNLINK_FAILURE;
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        let art = store.create(NS, Zeroizing::new("tok".to_owned())).await;
        flag.store(false, std::sync::atomic::Ordering::SeqCst);
        let art = art.unwrap();
        assert_eq!(art.retention, RetentionStatus::Incomplete);
        assert_eq!(
            std::fs::read(dir.path().join(&art.file_name)).unwrap(),
            b"dump"
        );
        for name in &old {
            assert!(dir.path().join(name).exists(), "{name} must survive");
        }
        // O próximo create (sem falha) converge para exatamente 3.
        let next = store
            .create(NS, Zeroizing::new("tok".to_owned()))
            .await
            .unwrap();
        assert_eq!(next.retention, RetentionStatus::Complete);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), KEEP_COPIES);
        assert!(dir.path().join(&next.file_name).exists());
    }

    fn drive_job(
        dir: &std::path::Path,
        control: &unix_fs::Control,
        deadline: std::time::Instant,
        after_link: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<BackupArtifact, SqldBackupError> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tx.try_send(unix_fs::Msg::Chunk(bytes::Bytes::from_static(b"data")))
            .unwrap();
        tx.try_send(unix_fs::Msg::Finish(4)).unwrap();
        unix_fs::run_job(dir, NS, control, deadline, rx, None, after_link)
    }

    fn far() -> std::time::Instant {
        std::time::Instant::now() + Duration::from_secs(60)
    }

    /// Cancelamento ANTES do commit: nada é publicado, sem temporário.
    #[test]
    fn cancel_before_commit_never_links_the_final() {
        let dir = private_dir();
        let control = unix_fs::Control::default();
        control.request_cancel();
        let err = drive_job(dir.path(), &control, far(), None).unwrap_err();
        assert_eq!(err, SqldBackupError::Cancelled);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// Prazo vencido antes do commit: nada é publicado (checagem do job).
    #[test]
    fn deadline_passed_before_commit_never_links_the_final() {
        let dir = private_dir();
        let control = unix_fs::Control::default();
        let past = std::time::Instant::now() - Duration::from_millis(1);
        let err = drive_job(dir.path(), &control, past, None).unwrap_err();
        assert_eq!(err, SqldBackupError::Timeout);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    /// Cancelamento DENTRO da janela de publicação (entre `linkat` e o fim):
    /// o job desfaz o final que acabou de criar.
    #[test]
    fn cancel_inside_the_commit_window_rolls_the_final_back() {
        let dir = private_dir();
        let control = Arc::new(unix_fs::Control::default());
        let hook_control = Arc::clone(&control);
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || hook_control.request_cancel());
        let err = drive_job(dir.path(), &control, far(), Some(hook)).unwrap_err();
        assert_eq!(err, SqldBackupError::Cancelled);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // Controle: sem cancelamento a mesma entrada publica.
        let control = unix_fs::Control::default();
        let art = drive_job(dir.path(), &control, far(), None).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(art.bytes, 4);
    }

    /// Prazo vencido DURANTE a publicação (depois do `linkat`, sem nenhum
    /// cancelamento assíncrono entregue): o job recusa o sucesso tardio,
    /// desfaz o final novo, não roda retenção e preserva os anteriores.
    #[test]
    fn deadline_crossed_during_publication_rolls_back_without_retention() {
        let dir = private_dir();
        let old: Vec<String> = (1..=4)
            .map(|n| seed(dir.path(), n, 1_000 + n as i64))
            .collect();
        let before = {
            let mut names: Vec<String> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        let control = unix_fs::Control::default();
        let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = Arc::clone(&reached);
        // Prazo de 1s (margem ampla para o setup); o hook, depois do
        // `linkat`, dorme além dele.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        let hook: Box<dyn FnOnce() + Send> = Box::new(move || {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(1500));
        });
        let err = drive_job(dir.path(), &control, deadline, Some(hook)).unwrap_err();
        assert!(
            reached.load(std::sync::atomic::Ordering::SeqCst),
            "the publication window (after linkat) must have been reached"
        );
        assert_eq!(err, SqldBackupError::Timeout);
        let after = {
            let mut names: Vec<String> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        };
        assert_eq!(
            after, before,
            "no new final, no temp, nothing retained away"
        );
        for name in &old {
            assert!(dir.path().join(name).exists(), "{name} must survive");
        }
        // Controle: a mesma entrada com prazo folgado publica (e retém).
        let art = drive_job(dir.path(), &control_fresh(), far(), None).unwrap();
        assert_eq!(art.bytes, 4);
        assert_eq!(art.retention, RetentionStatus::Complete);
    }

    fn control_fresh() -> unix_fs::Control {
        unix_fs::Control::default()
    }

    /// `Finish` com total diferente do gravado nunca publica.
    #[test]
    fn finish_total_mismatch_is_incomplete() {
        let dir = private_dir();
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tx.try_send(unix_fs::Msg::Chunk(bytes::Bytes::from_static(b"data")))
            .unwrap();
        tx.try_send(unix_fs::Msg::Finish(5)).unwrap();
        let control = unix_fs::Control::default();
        let err = unix_fs::run_job(dir.path(), NS, &control, far(), rx, None, None).unwrap_err();
        assert_eq!(err, SqldBackupError::Incomplete);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn backup_name_is_strict() {
        let uuid = "0b1f5a8e-6e1a-4c64-9d6b-1d3a0a9d2f10";
        assert!(parse_backup_name(&format!("{NS}-{uuid}.sql"), NS).is_some());
        assert!(parse_backup_name(&format!("{NS}-{uuid}.sql.bak"), NS).is_none());
        assert!(parse_backup_name(&format!("{NS}-{}.sql", uuid.to_uppercase()), NS).is_none());
        assert!(parse_backup_name(&format!("{NS}-{}.sql", uuid.replace('-', "")), NS).is_none());
        assert!(parse_backup_name(&format!("{NS}x{uuid}.sql"), NS).is_none());
        let other = format!("edger_{}", "f".repeat(64));
        assert!(parse_backup_name(&format!("{other}-{uuid}.sql"), NS).is_none());
    }
}
