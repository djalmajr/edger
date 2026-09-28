//! Autenticação por senha do plano de controle: usuário `root` semeado uma
//! vez, usuários adicionais criados pelo root (`role=operator`, sem papel
//! root delegável) e sessões opacas `ses-` persistentes.
//!
//! O store vive no MESMO arquivo SQLite das api-keys persistentes
//! (`EDGER_API_KEYS_DB`), em tabelas próprias (`console_users`,
//! `console_sessions`), com conexão separada e `busy_timeout` — o
//! `edger-core` continua sem I/O.
//!
//! Postura (falha fechada):
//! - Senha em Argon2id (m 19456 KiB, t 2, p 1 — parâmetros OWASP, os
//!   defaults do crate) com salt aleatório. O root token NUNCA é usado como
//!   senha e não existe senha default: o root só é semeado quando o usuário
//!   `root` NÃO EXISTE e `EDGER_ROOT_PASSWORD_FILE` aponta para arquivo
//!   legível com senha válida — operadores criados antes (pelo root token)
//!   não bloqueiam a semente, e o hash de um root existente nunca é
//!   sobrescrito. Usuários adicionais usam a mesma política de
//!   senha forte e só as permissões/escopos gravados (validados pelo mesmo
//!   catálogo de `validate_key_grant` das api-keys; `"*"` nunca é
//!   permissão atribuível). O username `root` é reservado e imutável.
//! - Sessão: token opaco de 32 bytes (`ses-` + base64url), persistido SOMENTE
//!   como SHA-256, TTL fixo de 7 dias na criação, revogação TERMINAL (a linha
//!   é removida — logout, troca de senha, desativação, redução de escopo,
//!   reset e exclusão revogam na mesma transação e nunca ressuscitam a
//!   sessão em restart). O principal da sessão é resolvido do estado ATUAL
//!   do usuário em cada request (sem cache): desativação e redução valem na
//!   hora.
//! - Os cálculos Argon2 (login, troca de senha, criação/reset de usuário)
//!   rodam na pool de blocking (`spawn_blocking`) sob um limite COMPARTILHADO
//!   de slots: cada hash reserva ~19 MiB, então nunca há fila ilimitada;
//!   sem slot no tempo do timeout a operação é negada (503), nunca
//!   autentica.
//! - Falha de consulta/escrita no store NUNCA autentica: a resposta é negação.
//! - Senha e token nunca são registrados em log.
//! - Falha de login (usuário inexistente, senha errada, usuário inativo)
//!   tem a MESMA resposta e o MESMO tempo (uma verificação Argon2, real ou
//!   dummy) — o oráculo de timing não revela qual usuário existe.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use edger_core::{root_principal, validate_key_grant, ApiKeyPrincipal, CoreError};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

/// Prefixo discriminante da sessão: disjunto de `egk_` (api-keys) e de JWT
/// (base64url de um JWT começa sempre com `eyJ`), então o dispatch por
/// prefixo no `ControlAuth` é ambiguo-zero.
pub const SESSION_PREFIX: &str = "ses-";

/// TTL fixo da sessão: 7 dias a partir da criação (não deslizante).
pub const SESSION_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Budget de tentativas de senha por IP REAL da conexão (`ConnectInfo` do
/// listener) — `X-Forwarded-For`/`X-Real-IP` do cliente nunca entram na chave
/// (forjáveis: girar o header não rotaciona o bucket). Janela deslizante de
/// 20 falhas em 15 min por IP, com teto de memória e sweep (buckets vazios
/// somem; estourado o teto, evicta o menos recente).
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);
const LOGIN_MAX_FAILURES: usize = 20;
const MAX_LIMITER_BUCKETS: usize = 10_000;

const USERNAME_MAX_LEN: usize = 64;
const PASSWORD_MAX_LEN: usize = 128;
const USERNAME_MIN_LEN: usize = 2;
const USERNAME_ALLOWED_LEN: usize = 32;

/// Limite compartilhado de cálculos Argon2 simultâneos (login, troca de
/// senha, criação/reset de usuário): cada hash reserva ~19 MiB (m=19456
/// KiB), então o teto também é teto de MEMÓRIA — não existe fila ilimitada
/// de hashes. Quem não pega slot no tempo do timeout é negado (503).
const HASH_CONCURRENCY: usize = 4;
const HASH_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

pub const ROOT_USERNAME: &str = "root";

/// Metadados de um usuário da console — SEM hash/senha: é o que o admin API
/// devolve (`{user:{...}}` / `{users:[...]}`) e o que a UI confia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleUserInfo {
    pub id: i64,
    pub username: String,
    /// `admin` para o root, `operator` para os demais (fixo pela API — não
    /// existe papel root delegável).
    pub role: String,
    pub is_root: bool,
    pub active: bool,
    pub permissions: Vec<String>,
    pub namespaces: Vec<String>,
    pub workers: Vec<String>,
    /// Epoch em segundos (contrato dos stores e do vocabulário do core).
    pub created_at: i64,
    pub updated_at: i64,
}

/// PATCH de usuário: campos opcionais — ausente (`None`) = mantém o atual.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConsoleUserPatch {
    pub disabled: Option<bool>,
    pub permissions: Option<Vec<String>>,
    pub namespaces: Option<Vec<String>>,
    pub workers: Option<Vec<String>>,
}

/// Janela deslizante de falhas de senha POR IP REAL (ver `LOGIN_WINDOW`).
/// A chave é o `IpAddr` da conexão (listener), nunca header do cliente.
#[derive(Debug)]
pub struct LoginLimiter {
    buckets: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
    window: Duration,
    max_failures: usize,
    max_buckets: usize,
}

impl LoginLimiter {
    pub fn new(window: Duration, max_failures: usize) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            window,
            max_failures,
            max_buckets: MAX_LIMITER_BUCKETS,
        }
    }

    fn default_policy() -> Self {
        Self::new(LOGIN_WINDOW, LOGIN_MAX_FAILURES)
    }

    /// `Some(retry_after)` quando o budget do IP já está esgotado.
    pub fn check(&self, ip: IpAddr) -> Option<Duration> {
        let mut buckets = self.lock();
        let now = Instant::now();
        // Sem bucket (IP nunca falhou): nunca cria aqui — o teto de memória
        // não pode ser estourado por quem só checa.
        let bucket = buckets.get_mut(&ip)?;
        while bucket
            .front()
            .is_some_and(|at| now.duration_since(*at) > self.window)
        {
            bucket.pop_front();
        }
        if bucket.len() < self.max_failures {
            return None;
        }
        let oldest = *bucket.front().expect("budget cheio tem front");
        let retry = self.window - now.duration_since(oldest);
        Some(retry.max(Duration::from_secs(1)))
    }

    pub fn record_failure(&self, ip: IpAddr) {
        let mut buckets = self.lock();
        let now = Instant::now();
        let bucket = buckets.entry(ip).or_default();
        while bucket
            .front()
            .is_some_and(|at| now.duration_since(*at) > self.window)
        {
            bucket.pop_front();
        }
        bucket.push_back(now);
        if buckets.len() >= self.max_buckets {
            self.sweep_locked(&mut buckets);
        }
    }

    /// Teto de memória: remove buckets vazios (sem falha na janela) e, ainda
    /// assim acima do teto, evicta os menos recentes (por última falha).
    fn sweep_locked(&self, buckets: &mut HashMap<IpAddr, VecDeque<Instant>>) {
        let now = Instant::now();
        buckets.retain(|_, bucket| {
            while bucket
                .front()
                .is_some_and(|at| now.duration_since(*at) > self.window)
            {
                bucket.pop_front();
            }
            !bucket.is_empty()
        });
        if buckets.len() <= self.max_buckets {
            return;
        }
        let excess = buckets.len() - self.max_buckets;
        let mut last_seen: Vec<(Instant, IpAddr)> = buckets
            .iter()
            .filter_map(|(ip, bucket)| bucket.back().map(|at| (*at, *ip)))
            .collect();
        last_seen.sort_by_key(|(at, _)| *at);
        for (_, ip) in last_seen.into_iter().take(excess) {
            buckets.remove(&ip);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, VecDeque<Instant>>> {
        self.buckets.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Política de senha forte (semente, troca e usuários adicionais): 12–128
/// runes, pelo menos uma letra, um dígito e um símbolo (nada
/// alfanumérico/espaço). Mesma política da referência Apigate; a semente do
/// boot, a nova senha da troca e a senha de um usuário criado/resetado
/// passam por ela — "inválida" significa fora dessa política.
pub fn validate_password_policy(password: &str) -> Result<(), CoreError> {
    let len = password.chars().count();
    if !(12..=PASSWORD_MAX_LEN).contains(&len) {
        return Err(policy_error());
    }
    let has_letter = password.chars().any(|c| c.is_alphabetic());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    let has_symbol = password
        .chars()
        .any(|c| !c.is_alphanumeric() && !c.is_whitespace());
    if has_letter && has_digit && has_symbol {
        Ok(())
    } else {
        Err(policy_error())
    }
}

fn policy_error() -> CoreError {
    CoreError::new(
        "VALIDATION_ERROR",
        format!(
            "password must be 12-{PASSWORD_MAX_LEN} characters and include a letter, a digit, and a symbol"
        ),
    )
}

/// Username da console: `[a-z0-9._-]`, 2–32 caracteres, início alfanumérico
/// (sem pontuação inicial) e `root` reservado (case-insensitive). A
/// unicidade é case-insensitive no banco (COLLATE NOCASE); por isso a
/// validação só aceita minúsculas — `Alice` é entrada inválida, não um
/// alias de `alice`.
pub fn validate_username(username: &str) -> Result<(), CoreError> {
    let len = username.chars().count();
    if !(USERNAME_MIN_LEN..=USERNAME_ALLOWED_LEN).contains(&len) {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            format!("username must be {USERNAME_MIN_LEN}-{USERNAME_ALLOWED_LEN} characters"),
        ));
    }
    if username
        .chars()
        .any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')))
    {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            "username must use only [a-z0-9._-]",
        ));
    }
    let first = username.chars().next().expect("username com 2+ caracteres");
    if !first.is_ascii_alphanumeric() {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            "username must start with a letter or digit",
        ));
    }
    if username.eq_ignore_ascii_case(ROOT_USERNAME) {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            "username 'root' is reserved",
        ));
    }
    Ok(())
}

/// Validação server-side do grant de um usuário adicional (antes de
/// gastar hash ou tocar no banco): username normalizado e não reservado,
/// entradas de escopo sem vazio/espaços nas pontas e permissões/escopos
/// pelo MESMO catálogo da criação de keys (`validate_key_grant` com
/// criador root) — `"*"` como PERMISSÃO é rejeitado lá (fora do catálogo),
/// enquanto `"*"` segue sendo escopo legítimo de namespaces/workers.
pub fn validate_user_grant(
    username: &str,
    permissions: &[String],
    namespaces: &[String],
    workers: &[String],
) -> Result<(), CoreError> {
    validate_username(username)?;
    for list in [permissions, namespaces, workers] {
        for entry in list {
            if entry.is_empty() || entry != entry.trim() {
                return Err(CoreError::new(
                    "VALIDATION_ERROR",
                    "scope entries must be non-empty without surrounding whitespace",
                ));
            }
        }
    }
    validate_key_grant(&root_principal(), permissions, namespaces, workers)
}

/// Lê a senha inicial no boot: arquivo inexistente/ilegível, vazio após trim
/// ou fora da política de força FALHA (o caller decide como falhar no boot).
/// Configurar `EDGER_ROOT_PASSWORD_FILE` sem arquivo válido não pode deixar a
/// instância subir sem senha usável.
pub fn load_seed_password(path: &Path) -> Result<Option<String>, CoreError> {
    let raw = std::fs::read_to_string(path).map_err(|err| {
        CoreError::new(
            "STORE_ERROR",
            format!("cannot read root password file {}: {err}", path.display()),
        )
    })?;
    let seed = raw.trim().to_string();
    if seed.is_empty() {
        return Err(CoreError::new("STORE_ERROR", "root password file is empty"));
    }
    validate_password_policy(&seed)?;
    Ok(Some(seed))
}

/// `true` quando o conjunto novo PERDEU algo do antigo (redução): deixar a
/// sessão viva com o principal antigo seria privilégio em cache.
fn shrink_contains(old: &[String], new: &[String]) -> bool {
    old.iter()
        .any(|entry| !new.iter().any(|candidate| candidate == entry))
}

/// Falhas de `login`/`change_password`. `InvalidCredentials` é GENÉRICA de
/// propósito (mesma resposta para usuário inexistente, senha errada e root
/// inativo) — as respostas não podem revelar se `root` existe.
#[derive(Debug)]
pub enum ConsoleAuthError {
    InvalidCredentials,
    /// Corpo/parâmetros fora do contrato (ex.: nova senha fraca).
    InvalidRequest(String),
    /// Budget de tentativas esgotado; carrega o `Retry-After`.
    RateLimited(Duration),
    /// Limite de cálculos de senha simultâneos esgotado (sem slot no tempo
    /// do timeout): o cálculo não anda — falha fechada (503 no handler).
    Busy,
    /// Falha de store — falha fechada (503 no handler; nunca autentica).
    Store(CoreError),
}

struct SessionRecord {
    user_id: i64,
}

struct UserRecord {
    id: i64,
    username: String,
    active: bool,
    role: String,
    permissions: Vec<String>,
    namespaces: Vec<String>,
    workers: Vec<String>,
    created_at: i64,
    updated_at: i64,
}

/// Metadados públicos a partir do registro (NUNCA hash/senha). O `role` do
/// root é sempre `admin` (o restante do fluxo depende dele).
fn user_info(record: &UserRecord) -> ConsoleUserInfo {
    let is_root = record.username.eq_ignore_ascii_case(ROOT_USERNAME);
    ConsoleUserInfo {
        id: record.id,
        username: record.username.clone(),
        role: if is_root {
            "admin".to_string()
        } else {
            record.role.clone()
        },
        is_root,
        active: record.active,
        permissions: record.permissions.clone(),
        namespaces: record.namespaces.clone(),
        workers: record.workers.clone(),
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

/// Argon2id nos parâmetros OWASP (m 19456 KiB, t 2, p 1, saída 32 B) — os
/// defaults do crate; declarados aqui para a intenção estar no lugar.
fn argon2() -> Argon2<'static> {
    Argon2::default()
}

fn hash_password(password: &str) -> Result<String, CoreError> {
    let salt = SaltString::generate(&mut OsRng);
    argon2()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| store_err(err.to_string()))
}

fn verify_password(password: &str, stored: &str) -> bool {
    match PasswordHash::new(stored) {
        Ok(hash) => argon2().verify_password(password.as_bytes(), &hash).is_ok(),
        Err(_) => false,
    }
}

/// Verificação dummy para igualar o tempo quando NÃO dá para (ou não deve)
/// comparar com a senha real: login de 401 leva o mesmo tempo com ou sem
/// root no banco (o oráculo de timing não revela existência de `root`).
fn dummy_verify(password: &str) {
    static DUMMY_HASH: OnceLock<String> = OnceLock::new();
    let dummy = DUMMY_HASH.get_or_init(|| {
        let salt = SaltString::generate(&mut OsRng);
        argon2()
            .hash_password(b"edger-dummy-password-verify", &salt)
            .map(|hash| hash.to_string())
            .expect("dummy hash")
    });
    let _ = verify_password(password, dummy);
}

/// Hash de token de sessão (mesmo contrato de `api_keys`, namespace próprio):
/// só o hash chega ao SQLite; o token bruto nunca é persistido.
fn hash_token(raw_token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"edger-console-v1:");
    hasher.update(raw_token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Token opaco: `ses-` + 32 bytes de CSPRNG em base64url (256 bits).
fn new_session_token() -> Result<String, CoreError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|err| store_err(err.to_string()))?;
    Ok(format!("{SESSION_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

pub struct SqliteConsoleStore {
    conn: Mutex<Connection>,
    /// Caminho do arquivo de banco (None em memória). Mesma pasta/arquivo do
    /// store de api-keys quando persistido.
    path: Option<PathBuf>,
}

impl SqliteConsoleStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let mut conn = Connection::open(path).map_err(store_err)?;
        // Dois stores escrevem no mesmo arquivo (api-keys + console): sem
        // busy timeout, uma escrita concorrente vira "database is locked".
        conn.pragma_update(None, "busy_timeout", 5_000)
            .map_err(store_err)?;
        Self::init_schema(&mut conn)?;
        Self::purge_expired(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: Some(path.to_path_buf()),
        })
    }

    pub fn in_memory() -> Result<Self, CoreError> {
        let mut conn = Connection::open_in_memory().map_err(store_err)?;
        Self::init_schema(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: None,
        })
    }

    /// Caminho do arquivo de banco, se o store persiste em arquivo.
    pub fn db_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn init_schema(conn: &mut Connection) -> Result<(), CoreError> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS console_users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE COLLATE NOCASE,
                password_hash TEXT NOT NULL,
                active INTEGER NOT NULL DEFAULT 1,
                role TEXT NOT NULL DEFAULT 'operator',
                is_root INTEGER NOT NULL DEFAULT 0,
                permissions TEXT NOT NULL DEFAULT '[]',
                namespaces TEXT NOT NULL DEFAULT '["*"]',
                workers TEXT NOT NULL DEFAULT '["*"]',
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                password_changed_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );
            CREATE TABLE IF NOT EXISTS console_sessions (
                id TEXT PRIMARY KEY,
                user_id INTEGER NOT NULL REFERENCES console_users(id),
                token_hash TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            "#,
        )
        .map_err(store_err)?;
        Self::migrate_users(conn)?;
        Ok(())
    }

    /// Migração idempotente dos bancos da fatia root (26.01): `CREATE TABLE
    /// IF NOT EXISTS` NÃO recria a tabela existente, então as colunas novas
    /// (papel/permissões/escopos/timestamp) entram por `ALTER TABLE` quando
    /// ausentes — sem recriar tabela, sem perder root/sessões. Defaults
    /// seguros: `role='operator'`, `is_root=0`, `permissions='[]'` (nenhuma
    /// capacidade) — e SOMENTE o registro `username='root'` recebe o estado
    /// completo de root (role admin, permissões `"*"`); nenhum outro registro
    /// é promovido.
    fn migrate_users(conn: &mut Connection) -> Result<(), CoreError> {
        let mut columns = HashSet::new();
        {
            let mut stmt = conn
                .prepare("PRAGMA table_info(console_users)")
                .map_err(store_err)?;
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(store_err)?;
            for column in rows {
                columns.insert(column.map_err(store_err)?);
            }
        }
        ensure_column(
            conn,
            &columns,
            "role",
            "role TEXT NOT NULL DEFAULT 'operator'",
        )?;
        ensure_column(
            conn,
            &columns,
            "is_root",
            "is_root INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            conn,
            &columns,
            "permissions",
            "permissions TEXT NOT NULL DEFAULT '[]'",
        )?;
        ensure_column(
            conn,
            &columns,
            "namespaces",
            "namespaces TEXT NOT NULL DEFAULT '[\"*\"]'",
        )?;
        ensure_column(
            conn,
            &columns,
            "workers",
            "workers TEXT NOT NULL DEFAULT '[\"*\"]'",
        )?;
        ensure_column(
            conn,
            &columns,
            "updated_at",
            "updated_at INTEGER NOT NULL DEFAULT 0",
        )?;
        // Estado completo do root da fatia anterior (idempotente: o UPDATE
        // é estável — reabrir não repete nem estoura a migração).
        conn.execute(
            "UPDATE console_users
             SET role = 'admin', is_root = 1, permissions = '[\"*\"]',
                 namespaces = '[\"*\"]', workers = '[\"*\"]', updated_at = created_at
             WHERE username = 'root'",
            [],
        )
        .map_err(store_err)?;
        // `updated_at` zerado (coluna nova em registro antigo) alinha ao
        // `created_at` — apenas registros sem timestamp de atualização.
        conn.execute(
            "UPDATE console_users SET updated_at = created_at WHERE updated_at = 0",
            [],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Limpeza em lote no boot (padrão Apigate): expiradas não sobrevivem ao
    /// restart.
    fn purge_expired(conn: &mut Connection) -> Result<usize, CoreError> {
        let now = now_epoch()? as i64;
        let purged = conn
            .execute(
                "DELETE FROM console_sessions WHERE expires_at <= ?1",
                params![now],
            )
            .map_err(store_err)?;
        if purged > 0 {
            tracing::info!(purged, "expired console sessions removed at boot");
        }
        Ok(purged)
    }

    /// Linha do root, se existir: `(id, password_hash, active)`.
    fn root_user(&self) -> Result<Option<(i64, String, bool)>, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare("SELECT id, password_hash, active FROM console_users WHERE username = ?1")
            .map_err(store_err)?;
        let mut rows = stmt
            .query_map(params![ROOT_USERNAME], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(store_err)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(store_err)?)),
            None => Ok(None),
        }
    }

    /// Linha do usuário pelo nome (case-insensitive via COLLATE NOCASE):
    /// `(id, password_hash, active)` — a âncora da prova de senha.
    fn user_by_username(&self, username: &str) -> Result<Option<(i64, String, bool)>, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare("SELECT id, password_hash, active FROM console_users WHERE username = ?1")
            .map_err(store_err)?;
        let mut rows = stmt
            .query_map(params![username], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(store_err)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(store_err)?)),
            None => Ok(None),
        }
    }

    /// Registro completo do usuário (estado ATUAL — sem cache): é dele que o
    /// principal da sessão é resolvido em cada request. Erro de parse do
    /// JSON de escopos é STORE_ERROR (falha fechada), nunca `Ok` parcial.
    fn user_by_id(&self, id: i64) -> Result<Option<UserRecord>, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        Self::user_by_id_locked(&conn, id)
    }

    fn user_by_id_locked(conn: &Connection, id: i64) -> Result<Option<UserRecord>, CoreError> {
        let raw = conn
            .query_row(
                "SELECT id, username, active, role, permissions, namespaces, workers,
                        created_at, updated_at
                 FROM console_users WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? == 1,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .ok();
        let Some((
            id,
            username,
            active,
            role,
            permissions,
            namespaces,
            workers,
            created_at,
            updated_at,
        )) = raw
        else {
            return Ok(None);
        };
        Ok(Some(UserRecord {
            id,
            username,
            active,
            role,
            permissions: parse_scopes(&permissions)?,
            namespaces: parse_scopes(&namespaces)?,
            workers: parse_scopes(&workers)?,
            created_at,
            updated_at,
        }))
    }

    /// Metadados de todos os usuários (NUNCA hash/senha), ordenados por id.
    fn list_users(&self) -> Result<Vec<ConsoleUserInfo>, CoreError> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, username, active, role, permissions, namespaces, workers,
                        created_at, updated_at
                 FROM console_users ORDER BY id",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? == 1,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            })
            .map_err(store_err)?;
        let mut users = Vec::new();
        for row in rows {
            let (
                id,
                username,
                active,
                role,
                permissions,
                namespaces,
                workers,
                created_at,
                updated_at,
            ) = row.map_err(store_err)?;
            let permissions = parse_scopes(&permissions)?;
            let namespaces = parse_scopes(&namespaces)?;
            let workers = parse_scopes(&workers)?;
            let is_root = username.eq_ignore_ascii_case(ROOT_USERNAME);
            users.push(ConsoleUserInfo {
                id,
                username,
                role: if is_root { "admin".to_string() } else { role },
                is_root,
                active,
                permissions,
                namespaces,
                workers,
                created_at,
                updated_at,
            });
        }
        Ok(users)
    }

    fn insert_root(&self, password: &str) -> Result<(), CoreError> {
        let hash = hash_password(password)?;
        let now = now_epoch()? as i64;
        let all = json_array(&["*".to_string()])?;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            "INSERT INTO console_users
             (username, password_hash, active, role, is_root, permissions, namespaces, workers,
              created_at, updated_at, password_changed_at)
             VALUES (?1, ?2, 1, 'admin', 1, ?3, ?3, ?3, ?4, ?4, ?4)",
            params![ROOT_USERNAME, hash, all, now],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Criação de usuário adicional (sempre `role=operator`, `is_root=0`):
    /// a unicidade case-insensitive do username é garantida pelo UNIQUE
    /// COLLATE NOCASE — conflito é `USERNAME_TAKEN` (409), nunca erro vago.
    fn create_user_record(
        &self,
        username: &str,
        password_hash: &str,
        permissions: &[String],
        namespaces: &[String],
        workers: &[String],
        now: i64,
    ) -> Result<ConsoleUserInfo, CoreError> {
        let permissions_json = json_array(permissions)?;
        let namespaces_json = json_array(namespaces)?;
        let workers_json = json_array(workers)?;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            "INSERT INTO console_users
             (username, password_hash, active, role, is_root, permissions, namespaces, workers,
              created_at, updated_at, password_changed_at)
             VALUES (?1, ?2, 1, 'operator', 0, ?3, ?4, ?5, ?6, ?6, ?6)",
            params![
                username,
                password_hash,
                permissions_json,
                namespaces_json,
                workers_json,
                now
            ],
        )
        .map_err(|err| {
            if err.to_string().contains("UNIQUE constraint failed") {
                username_taken_err()
            } else {
                store_err(err.to_string())
            }
        })?;
        let id = conn.last_insert_rowid();
        Self::user_by_id_locked(&conn, id)
            .map(|user| user.expect("linha recém-criada existe"))
            .map(|record| user_info(&record))
    }

    /// PATCH transacional (atualiza `console_users` E ANTES disso lê o
    /// estado atual DENTRO da transação — sem TOCTOU): o root nunca é alvo
    /// (`USER_IMMUTABLE`); o conjunto FINAL precisa passar no catálogo de
    /// grant (lista vazia/inválida falha SEM alterar nada); e as sessões do
    /// usuário morrem na mesma transação quando a mudança o desativa ou
    /// REDUZ permissões/escopos (expansão não revoga: o principal da sessão
    /// já é resolvido do estado atual).
    fn update_user_tx(
        &self,
        id: i64,
        patch: &ConsoleUserPatch,
        now: i64,
    ) -> Result<ConsoleUserInfo, CoreError> {
        let mut conn = self.conn.lock().map_err(|_| lock_err())?;
        let tx = conn.transaction().map_err(store_err)?;
        let user = tx
            .query_row(
                "SELECT username, active, permissions, namespaces, workers
                 FROM console_users WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? == 1,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .map_err(|_| user_not_found_err(id))?;
        let (username, active, old_permissions_raw, old_namespaces_raw, old_workers_raw) = user;
        let old_permissions = parse_scopes(&old_permissions_raw)?;
        let old_namespaces = parse_scopes(&old_namespaces_raw)?;
        let old_workers = parse_scopes(&old_workers_raw)?;
        if username.eq_ignore_ascii_case(ROOT_USERNAME) {
            tx.rollback().map_err(store_err)?;
            return Err(user_immutable_err());
        }
        let permissions = patch
            .permissions
            .clone()
            .unwrap_or_else(|| old_permissions.clone());
        let namespaces = patch
            .namespaces
            .clone()
            .unwrap_or_else(|| old_namespaces.clone());
        let workers = patch.workers.clone().unwrap_or_else(|| old_workers.clone());
        let active = match patch.disabled {
            Some(disabled) => !disabled,
            None => active,
        };
        // O conjunto FINAL (o que fica gravado) passa no MESMO catálogo da
        // criação: um PATCH que deixaria permissões vazias ou inválidas
        // falha com rollback integral.
        validate_key_grant(&root_principal(), &permissions, &namespaces, &workers)?;
        let revoke = !active
            || shrink_contains(&old_permissions, &permissions)
            || shrink_contains(&old_namespaces, &namespaces)
            || shrink_contains(&old_workers, &workers);
        tx.execute(
            "UPDATE console_users
             SET active = ?1, permissions = ?2, namespaces = ?3, workers = ?4, updated_at = ?5
             WHERE id = ?6",
            params![
                active as i64,
                json_array(&permissions)?,
                json_array(&namespaces)?,
                json_array(&workers)?,
                now,
                id
            ],
        )
        .map_err(store_err)?;
        if revoke {
            tx.execute(
                "DELETE FROM console_sessions WHERE user_id = ?1",
                params![id],
            )
            .map_err(store_err)?;
        }
        let updated =
            Self::user_by_id_locked(&tx, id).map(|user| user.expect("linha atualizada existe"))?;
        tx.commit().map_err(store_err)?;
        Ok(user_info(&updated))
    }

    /// Reset de senha transacional (UPDATE de `console_users` E revogação de
    /// TODAS as sessões do alvo na mesma transação — nunca estado parcial).
    fn reset_user_password_tx(
        &self,
        id: i64,
        new_hash: &str,
        now: i64,
    ) -> Result<ConsoleUserInfo, CoreError> {
        let mut conn = self.conn.lock().map_err(|_| lock_err())?;
        let tx = conn.transaction().map_err(store_err)?;
        let username = match tx.query_row(
            "SELECT username FROM console_users WHERE id = ?1",
            params![id],
            |row| row.get::<_, String>(0),
        ) {
            Ok(username) => username,
            Err(_) => {
                tx.rollback().map_err(store_err)?;
                return Err(user_not_found_err(id));
            }
        };
        if username.eq_ignore_ascii_case(ROOT_USERNAME) {
            tx.rollback().map_err(store_err)?;
            return Err(user_immutable_err());
        }
        tx.execute(
            "UPDATE console_users
             SET password_hash = ?1, password_changed_at = ?2, updated_at = ?2
             WHERE id = ?3",
            params![new_hash, now, id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM console_sessions WHERE user_id = ?1",
            params![id],
        )
        .map_err(store_err)?;
        let updated =
            Self::user_by_id_locked(&tx, id).map(|user| user.expect("linha do reset existe"))?;
        tx.commit().map_err(store_err)?;
        Ok(user_info(&updated))
    }

    /// Exclusão transacional: sessões E usuário saem na mesma transação
    /// (o root nunca é alvo). FK não é enforced no SQLite (default), então a
    /// ordem garante que não sobra sessão órfã; mesmo assim o lookup de
    /// sessão falha fechado para `user_id` sem usuário.
    fn delete_user_tx(&self, id: i64) -> Result<(), CoreError> {
        let mut conn = self.conn.lock().map_err(|_| lock_err())?;
        let tx = conn.transaction().map_err(store_err)?;
        let username = match tx.query_row(
            "SELECT username FROM console_users WHERE id = ?1",
            params![id],
            |row| row.get::<_, String>(0),
        ) {
            Ok(username) => username,
            Err(_) => {
                tx.rollback().map_err(store_err)?;
                return Err(user_not_found_err(id));
            }
        };
        if username.eq_ignore_ascii_case(ROOT_USERNAME) {
            tx.rollback().map_err(store_err)?;
            return Err(user_immutable_err());
        }
        tx.execute(
            "DELETE FROM console_sessions WHERE user_id = ?1",
            params![id],
        )
        .map_err(store_err)?;
        tx.execute("DELETE FROM console_users WHERE id = ?1", params![id])
            .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(())
    }

    /// PROVA da senha do `username` pedido (fora de transação de escrita —
    /// Argon2 não segura lock): `Some((user_id, hash))` quando o usuário
    /// existe, está ativo e a senha bate; o hash devolvido é a âncora que a
    /// emissão da sessão reconfirma. Os demais caminhos (usuário inexistente,
    /// inativo, senha errada) rodam UMA verificação Argon2 (real ou dummy)
    /// e devolvem `None` — mesma resposta e mesmo tempo, sem oráculo de
    /// usuário (e sem revelar se `root` existe).
    fn prove_password(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<(i64, String)>, CoreError> {
        match self.user_by_username(username)? {
            Some((id, hash, true)) => Ok(verify_password(password, &hash).then_some((id, hash))),
            _ => {
                dummy_verify(password);
                Ok(None)
            }
        }
    }

    /// PROVA da senha atual do DONO da sessão (troca de senha por qualquer
    /// usuário de sessão, não só root): a sessão precisa existir, estar viva
    /// e o usuário ativo. Demais caminhos rodam o dummy e devolvem `None`
    /// (genérico, sem oráculo); sessão expirada é limpa como no lookup.
    fn prove_session_user_password(
        &self,
        session_token: &str,
        current: &str,
    ) -> Result<Option<String>, CoreError> {
        let hash = hash_token(session_token);
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let session: Option<(i64, i64)> = conn
            .query_row(
                "SELECT user_id, expires_at FROM console_sessions WHERE token_hash = ?1",
                params![hash],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .ok();
        let Some((user_id, expires_at)) = session else {
            dummy_verify(current);
            return Ok(None);
        };
        let now = now_epoch()?;
        if (expires_at as u64) <= now {
            conn.execute(
                "DELETE FROM console_sessions WHERE token_hash = ?1",
                params![hash],
            )
            .map_err(store_err)?;
            dummy_verify(current);
            return Ok(None);
        }
        let user: Option<(bool, String)> = conn
            .query_row(
                "SELECT active, password_hash FROM console_users WHERE id = ?1",
                params![user_id],
                |row| Ok((row.get::<_, i64>(0)? == 1, row.get::<_, String>(1)?)),
            )
            .ok();
        let Some((active, stored)) = user else {
            dummy_verify(current);
            return Ok(None);
        };
        if !active {
            dummy_verify(current);
            return Ok(None);
        }
        Ok(verify_password(current, &stored).then_some(stored))
    }

    /// Emissão da sessão DENTRO de uma transação que RECONFIRMA a prova:
    /// usuário ainda ativo e hash da senha IGUAL ao que foi provado. Se mudou
    /// (troca de senha concorrente entre provar e emitir), rollback e
    /// `PROOF_STALE` — a sessão nunca nasce com senha antiga.
    fn issue_session_if_proof_still_valid(
        &self,
        user_id: i64,
        expected_hash: &str,
        raw_token: &str,
        created_at: u64,
        expires_at: u64,
    ) -> Result<(), CoreError> {
        let mut conn = self.conn.lock().map_err(|_| lock_err())?;
        let tx = conn.transaction().map_err(store_err)?;
        let (hash, active): (String, bool) = tx
            .query_row(
                "SELECT password_hash, active FROM console_users WHERE id = ?1",
                params![user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| proof_stale_err())?;
        if !active || hash != expected_hash {
            tx.rollback().map_err(store_err)?;
            return Err(proof_stale_err());
        }
        tx.execute(
            "INSERT INTO console_sessions (id, user_id, token_hash, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                Uuid::new_v4().simple().to_string(),
                user_id,
                hash_token(raw_token),
                created_at as i64,
                expires_at as i64
            ],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(())
    }

    /// Sessão viva para o token (expirada vira DELETE — não ressuscita em
    /// restart). Erro de DB é `Err`, nunca `Ok(None)` silencioso disfarçado
    /// de "não existe": o caller falha fechado.
    fn lookup_session(&self, raw_token: &str) -> Result<Option<SessionRecord>, CoreError> {
        let hash = hash_token(raw_token);
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare("SELECT user_id, expires_at FROM console_sessions WHERE token_hash = ?1")
            .map_err(store_err)?;
        let mut rows = stmt
            .query_map(params![hash], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(store_err)?;
        let row = rows.next().transpose().map_err(store_err)?;
        let Some((user_id, expires_at)) = row else {
            return Ok(None);
        };
        let now = now_epoch()?;
        if (expires_at as u64) <= now {
            conn.execute(
                "DELETE FROM console_sessions WHERE token_hash = ?1",
                params![hash],
            )
            .map_err(store_err)?;
            return Ok(None);
        }
        Ok(Some(SessionRecord { user_id }))
    }

    /// `true` quando a sessão existia e foi removida (revogação é terminal).
    fn delete_session(&self, raw_token: &str) -> Result<bool, CoreError> {
        let hash = hash_token(raw_token);
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let removed = conn
            .execute(
                "DELETE FROM console_sessions WHERE token_hash = ?1",
                params![hash],
            )
            .map_err(store_err)?;
        Ok(removed > 0)
    }

    /// Troca atômica da senha do dono da sessão + rotação de sessões, com
    /// REVALIDAÇÃO dentro da transação: só altera se (1) a SESSÃO SOLICITANTE
    /// ainda existe e está viva, (2) o usuário está ativo e (3) o hash atual
    /// ainda é o hash provado. Qualquer divergência: rollback integral
    /// (`PROOF_STALE`), o banco não muda.
    fn change_password_and_reissue(
        &self,
        requesting_session: &str,
        expected_hash: &str,
        new_hash: &str,
        raw_token: &str,
        now: i64,
    ) -> Result<(), CoreError> {
        let mut conn = self.conn.lock().map_err(|_| lock_err())?;
        let tx = conn.transaction().map_err(store_err)?;
        // (1) A sessão que pediu a troca continua viva (não saiu por logout,
        // revogação ou expiração entre a prova e o commit).
        let (user_id, expires_at): (i64, i64) = tx
            .query_row(
                "SELECT user_id, expires_at FROM console_sessions WHERE token_hash = ?1",
                params![hash_token(requesting_session)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| proof_stale_err())?;
        if expires_at <= now {
            // Expirada no meio do caminho: limpa e falha fechado.
            let _ = tx.execute(
                "DELETE FROM console_sessions WHERE token_hash = ?1",
                params![hash_token(requesting_session)],
            );
            tx.rollback().map_err(store_err)?;
            return Err(proof_stale_err());
        }
        // (2)+(3) Usuário ativo e hash ainda é o provado.
        let (hash, active): (String, bool) = tx
            .query_row(
                "SELECT password_hash, active FROM console_users WHERE id = ?1",
                params![user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| proof_stale_err())?;
        if !active || hash != expected_hash {
            tx.rollback().map_err(store_err)?;
            return Err(proof_stale_err());
        }
        // Aplica de uma vez: novo hash, todas as sessões morrem (a corrente
        // inclusive) e a nova entra.
        tx.execute(
            "UPDATE console_users SET password_hash = ?1, password_changed_at = ?2 WHERE id = ?3",
            params![new_hash, now, user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM console_sessions WHERE user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "INSERT INTO console_sessions (id, user_id, token_hash, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                Uuid::new_v4().simple().to_string(),
                user_id,
                hash_token(raw_token),
                now,
                now + SESSION_TTL.as_secs() as i64
            ],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(())
    }
}

/// Serviço que o `ControlAuth` e os handlers enxergam: login/logout/troca de
/// senha + autenticação de sessão + gestão de usuários adicionais
/// (root-only) + disponibilidade (sem segredos). Os cálculos Argon2 têm um
/// limite compartilhado de slots (`hash_slots`) — nunca fila ilimitada.
pub struct ConsoleAuthService {
    store: Arc<SqliteConsoleStore>,
    limiter: LoginLimiter,
    /// Limite compartilhado de cálculos Argon2 simultâneos (login, troca de
    /// senha, criação/reset de usuário): cada hash reserva ~19 MiB, então o
    /// teto também é teto de memória.
    hash_slots: Arc<Semaphore>,
    hash_acquire_timeout: Duration,
}

impl ConsoleAuthService {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        Ok(Self {
            store: Arc::new(SqliteConsoleStore::open(path)?),
            limiter: LoginLimiter::default_policy(),
            hash_slots: Arc::new(Semaphore::new(HASH_CONCURRENCY)),
            hash_acquire_timeout: HASH_ACQUIRE_TIMEOUT,
        })
    }

    pub fn in_memory() -> Result<Self, CoreError> {
        Ok(Self {
            store: Arc::new(SqliteConsoleStore::in_memory()?),
            limiter: LoginLimiter::default_policy(),
            hash_slots: Arc::new(Semaphore::new(HASH_CONCURRENCY)),
            hash_acquire_timeout: HASH_ACQUIRE_TIMEOUT,
        })
    }

    /// Limitador configurável (janela + budget) — usado por testes que
    /// precisam esgotar o budget sem pagar dezenas de Argon2.
    pub fn in_memory_with_limiter(
        window: Duration,
        max_failures: usize,
    ) -> Result<Self, CoreError> {
        Ok(Self {
            store: Arc::new(SqliteConsoleStore::in_memory()?),
            limiter: LoginLimiter::new(window, max_failures),
            hash_slots: Arc::new(Semaphore::new(HASH_CONCURRENCY)),
            hash_acquire_timeout: HASH_ACQUIRE_TIMEOUT,
        })
    }

    /// Concorrência máxima de cálculos de senha (testes do limite).
    pub fn with_hash_concurrency(mut self, slots: usize) -> Self {
        self.hash_slots = Arc::new(Semaphore::new(slots.max(1)));
        self
    }

    /// Timeout para conseguir um slot de cálculo (testes do limite).
    pub fn with_hash_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.hash_acquire_timeout = timeout;
        self
    }

    /// Slots compartilhados de cálculo de senha (testes do limite).
    pub fn hash_slots(&self) -> &Arc<Semaphore> {
        &self.hash_slots
    }

    /// Caminho do arquivo de banco do store, se persistir em arquivo.
    pub fn db_path(&self) -> Option<PathBuf> {
        self.store.db_path().map(|path| path.to_path_buf())
    }

    /// Limitador global de tentativas de senha (login e troca compartilham).
    pub fn limiter(&self) -> &LoginLimiter {
        &self.limiter
    }

    /// Semeia `root` SOMENTE quando o usuário `root` AINDA NÃO EXISTE e há
    /// senha de semente — operadores criados antes (pelo root token) NÃO
    /// bloqueiam a semente (a regra não é "banco vazio"). O hash de um root
    /// existente NUNCA é sobrescrito (nova semente em root presente é
    /// ignorada). `false` = não semeou (root já existe ou sem semente) —
    /// nunca erro.
    pub fn seed_root_if_empty(&self, password: Option<&str>) -> Result<bool, CoreError> {
        if self.store.root_user()?.is_some() {
            return Ok(false);
        }
        let Some(password) = password else {
            return Ok(false);
        };
        validate_password_policy(password)?;
        self.store.insert_root(password)?;
        Ok(true)
    }

    /// Existe usuário `root` no store (a semente do boot fechou o gate de
    /// open mode; a UI usa para saber se o modo senha existe).
    pub fn has_root_user(&self) -> Result<bool, CoreError> {
        Ok(self.store.root_user()?.is_some())
    }

    /// Autenticação por senha disponível: root existe E está ativo.
    pub fn password_enabled(&self) -> Result<bool, CoreError> {
        match self.store.root_user()? {
            Some((_, _, active)) => Ok(active),
            None => Ok(false),
        }
    }

    /// Login: budget por IP real, PROVA da senha (Argon2, custo igualado —
    /// real ou dummy) e emissão da sessão em transação que reconfirma a
    /// prova. Funciona para o root E para usuários adicionais (username
    /// case-insensitive). A senha NUNCA aparece em log.
    pub fn login(
        &self,
        client_ip: IpAddr,
        username: &str,
        password: &str,
    ) -> Result<String, ConsoleAuthError> {
        if let Some(retry) = self.limiter.check(client_ip) {
            return Err(ConsoleAuthError::RateLimited(retry));
        }
        let username = username.trim();
        if let Err(message) = check_login_input(username, password) {
            return Err(ConsoleAuthError::InvalidRequest(message));
        }
        // Fase 1: provar a senha FORA da transação de escrita.
        self.finish_login(client_ip, self.store.prove_password(username, password))
    }

    /// Login via HTTP: a prova Argon2 roda na pool de blocking (`spawn_blocking`)
    /// SOB O LIMITE COMPARTILHADO de slots de hash — nunca na thread do
    /// runtime, nunca sem limite. Sem slot no tempo do timeout: `Busy`
    /// (503 no handler — falha fechada, o cálculo não anda).
    pub async fn login_async(
        &self,
        client_ip: IpAddr,
        username: &str,
        password: &str,
    ) -> Result<String, ConsoleAuthError> {
        if let Some(retry) = self.limiter.check(client_ip) {
            return Err(ConsoleAuthError::RateLimited(retry));
        }
        let username = username.trim();
        if let Err(message) = check_login_input(username, password) {
            return Err(ConsoleAuthError::InvalidRequest(message));
        }
        let username = username.to_string();
        let password = password.to_string();
        let store = Arc::clone(&self.store);
        let permit = match self.acquire_hash_slot().await {
            Ok(permit) => permit,
            Err(_) => return Err(ConsoleAuthError::Busy),
        };
        let proof = tokio::task::spawn_blocking(move || store.prove_password(&username, &password))
            .await
            .map_err(|err| {
                CoreError::new("STORE_ERROR", format!("password proof task failed: {err}"))
            });
        drop(permit);
        let proof = proof.map_err(ConsoleAuthError::Store)?;
        self.finish_login(client_ip, proof)
    }

    /// Fase 2 do login: token novo + emissão reconfirmando a prova (usuário
    /// ativo + hash idêntico). Se a senha mudou entre as fases, falha
    /// genérica SEM sessão.
    fn finish_login(
        &self,
        client_ip: IpAddr,
        proof: Result<Option<(i64, String)>, CoreError>,
    ) -> Result<String, ConsoleAuthError> {
        let (user_id, expected_hash) = match proof {
            Ok(Some(proof)) => proof,
            Ok(None) => {
                self.limiter.record_failure(client_ip);
                return Err(ConsoleAuthError::InvalidCredentials);
            }
            Err(err) => return Err(ConsoleAuthError::Store(err)),
        };
        let token = new_session_token().map_err(ConsoleAuthError::Store)?;
        let now = now_epoch().map_err(ConsoleAuthError::Store)?;
        match self.store.issue_session_if_proof_still_valid(
            user_id,
            &expected_hash,
            &token,
            now,
            now + SESSION_TTL.as_secs(),
        ) {
            Ok(()) => Ok(token),
            Err(err) if err.code == "PROOF_STALE" => {
                self.limiter.record_failure(client_ip);
                Err(ConsoleAuthError::InvalidCredentials)
            }
            Err(err) => Err(ConsoleAuthError::Store(err)),
        }
    }

    /// Sessão -> principal a partir do estado ATUAL do usuário (a linha é
    /// relida em cada request — sem cache): desativação ou redução de escopo
    /// valem na hora, sem principal antigo com permissões ampliadas. O root
    /// resolve sempre para `root_principal()` (contrato da fatia root);
    /// usuários adicionais carregam exatamente as permissões/escopos
    /// gravados, com `is_root=false`.
    pub fn authenticate_session(&self, raw_token: &str) -> Option<ApiKeyPrincipal> {
        if !raw_token.starts_with(SESSION_PREFIX) {
            return None;
        }
        let record = match self.store.lookup_session(raw_token) {
            Ok(record) => record?,
            Err(err) => {
                tracing::warn!(
                    code = %err.code,
                    "console session lookup failed: {}", err.message
                );
                return None;
            }
        };
        let user = match self.store.user_by_id(record.user_id) {
            Ok(user) => user?,
            Err(err) => {
                tracing::warn!(
                    code = %err.code,
                    "console user lookup failed: {}", err.message
                );
                return None;
            }
        };
        if !user.active {
            return None;
        }
        if user.username.eq_ignore_ascii_case(ROOT_USERNAME) {
            return Some(root_principal());
        }
        Some(ApiKeyPrincipal {
            id: user.id as u64,
            name: user.username.clone(),
            key_prefix: "ses".into(),
            role: user.role.clone(),
            permissions: user.permissions.clone(),
            namespaces: user.namespaces.clone(),
            workers: user.workers.clone(),
            is_root: false,
            expires_at: None,
        })
    }

    /// Revoga a sessão do token (terminal: a linha sai do banco). `false` =
    /// não era sessão viva.
    pub fn logout(&self, raw_token: &str) -> Result<bool, CoreError> {
        if !raw_token.starts_with(SESSION_PREFIX) {
            return Ok(false);
        }
        self.store.delete_session(raw_token)
    }

    /// Troca a senha do dono da sessão: valida a atual (prova contra o
    /// usuário da sessão, mesmo budget por IP do login) e a nova (política
    /// forte), rotaciona TODAS as sessões do usuário e devolve o NOVO token.
    /// A transação revalida a sessão solicitante e o hash provado; falha não
    /// altera o banco. Funciona para qualquer usuário de sessão (root ou
    /// adicional) — sem depender de nome root.
    pub fn change_password(
        &self,
        client_ip: IpAddr,
        session_token: &str,
        current: &str,
        new: &str,
    ) -> Result<String, ConsoleAuthError> {
        if let Some(retry) = self.limiter.check(client_ip) {
            return Err(ConsoleAuthError::RateLimited(retry));
        }
        if current.is_empty() || current.chars().count() > PASSWORD_MAX_LEN {
            return Err(ConsoleAuthError::InvalidRequest(
                "current password is required".into(),
            ));
        }
        validate_password_policy(new)
            .map_err(|err| ConsoleAuthError::InvalidRequest(err.message.clone()))?;
        // Fase 1: provar a senha atual do dono da sessão e capturar o
        // hash-âncora.
        let expected_hash = match self
            .store
            .prove_session_user_password(session_token, current)
        {
            Ok(Some(hash)) => hash,
            Ok(None) => {
                self.limiter.record_failure(client_ip);
                return Err(ConsoleAuthError::InvalidCredentials);
            }
            Err(err) => return Err(ConsoleAuthError::Store(err)),
        };
        let new_hash = hash_password(new).map_err(ConsoleAuthError::Store)?;
        self.change_password_finalize(session_token, &expected_hash, &new_hash)
    }

    /// Troca de senha via HTTP (qualquer usuário de sessão): as DUAS
    /// operações Argon2 (verificar a atual + hash da nova) rodam na pool de
    /// blocking sob UM slot do limite compartilhado; o commit final (levo)
    /// fica fora do slot. Sem slot no tempo do timeout: `Busy`.
    pub async fn change_password_async(
        &self,
        client_ip: IpAddr,
        session_token: &str,
        current: &str,
        new: &str,
    ) -> Result<String, ConsoleAuthError> {
        if let Some(retry) = self.limiter.check(client_ip) {
            return Err(ConsoleAuthError::RateLimited(retry));
        }
        if current.is_empty() || current.chars().count() > PASSWORD_MAX_LEN {
            return Err(ConsoleAuthError::InvalidRequest(
                "current password is required".into(),
            ));
        }
        validate_password_policy(new)
            .map_err(|err| ConsoleAuthError::InvalidRequest(err.message.clone()))?;
        let session_token = session_token.to_string();
        let current = current.to_string();
        let new = new.to_string();
        let session_owned = session_token.to_string();
        let store = Arc::clone(&self.store);
        let permit = match self.acquire_hash_slot().await {
            Ok(permit) => permit,
            Err(_) => return Err(ConsoleAuthError::Busy),
        };
        let heavy =
            tokio::task::spawn_blocking(move || -> Result<Option<(String, String)>, CoreError> {
                let expected_hash =
                    match store.prove_session_user_password(&session_owned, &current)? {
                        Some(hash) => hash,
                        None => return Ok(None),
                    };
                Ok(Some((expected_hash, hash_password(&new)?)))
            })
            .await
            .map_err(|err| {
                CoreError::new("STORE_ERROR", format!("password change task failed: {err}"))
            });
        drop(permit);
        let heavy = heavy.map_err(ConsoleAuthError::Store)?;
        let Some((expected_hash, new_hash)) = heavy.map_err(ConsoleAuthError::Store)? else {
            self.limiter.record_failure(client_ip);
            return Err(ConsoleAuthError::InvalidCredentials);
        };
        self.change_password_finalize(&session_token, &expected_hash, &new_hash)
    }

    /// Fase 2 da troca: token novo + transação que revalida a sessão
    /// solicitante e a prova ANTES de qualquer escrita (sessão
    /// revogada/expirada ou hash trocado entre as fases => falha genérica,
    /// banco intacto).
    fn change_password_finalize(
        &self,
        session_token: &str,
        expected_hash: &str,
        new_hash: &str,
    ) -> Result<String, ConsoleAuthError> {
        let token = new_session_token().map_err(ConsoleAuthError::Store)?;
        let now = now_epoch().map_err(ConsoleAuthError::Store)? as i64;
        match self.store.change_password_and_reissue(
            session_token,
            expected_hash,
            new_hash,
            &token,
            now,
        ) {
            Ok(()) => Ok(token),
            Err(err) if err.code == "PROOF_STALE" => Err(ConsoleAuthError::InvalidCredentials),
            Err(err) => Err(ConsoleAuthError::Store(err)),
        }
    }

    // ------------------------------------------------------------------
    // Gestão de usuários adicionais (root-only via HTTP): metadados SEM
    // hash/senha, transações que revogam sessões na mesma escrita e
    // validação de catálogo/escopos ANTES de gastar hash ou persistir.
    // ------------------------------------------------------------------

    pub fn list_users(&self) -> Result<Vec<ConsoleUserInfo>, CoreError> {
        self.store.list_users()
    }

    /// Criação: valida nome/senha/permissões (falha 400 sem gastar hash),
    /// faz o hash sob o limite compartilhado e insere; username duplicado é
    /// `USERNAME_TAKEN` (409). O novo usuário é sempre `role=operator`.
    pub async fn create_user(
        &self,
        username: &str,
        password: &str,
        permissions: &[String],
        namespaces: &[String],
        workers: &[String],
    ) -> Result<ConsoleUserInfo, CoreError> {
        validate_user_grant(username, permissions, namespaces, workers)?;
        validate_password_policy(password)?;
        let password_hash = self.hash_password_limited(password).await?;
        let now = now_epoch()? as i64;
        self.store.create_user_record(
            username,
            &password_hash,
            permissions,
            namespaces,
            workers,
            now,
        )
    }

    pub fn update_user(
        &self,
        id: i64,
        patch: &ConsoleUserPatch,
    ) -> Result<ConsoleUserInfo, CoreError> {
        let now = now_epoch()? as i64;
        self.store.update_user_tx(id, patch, now)
    }

    pub async fn reset_user_password(
        &self,
        id: i64,
        password: &str,
    ) -> Result<ConsoleUserInfo, CoreError> {
        validate_password_policy(password)?;
        let new_hash = self.hash_password_limited(password).await?;
        let now = now_epoch()? as i64;
        self.store.reset_user_password_tx(id, &new_hash, now)
    }

    pub fn delete_user(&self, id: i64) -> Result<(), CoreError> {
        self.store.delete_user_tx(id)
    }

    /// Slot do limite compartilhado de cálculos Argon2; sem slot no tempo do
    /// timeout, `CONSOLE_BUSY` (o cálculo não anda — nunca autentica).
    async fn acquire_hash_slot(&self) -> Result<OwnedSemaphorePermit, CoreError> {
        match tokio::time::timeout(
            self.hash_acquire_timeout,
            Arc::clone(&self.hash_slots).acquire_owned(),
        )
        .await
        {
            Ok(permit) => permit.map_err(|err| {
                CoreError::new("CONSOLE_BUSY", format!("hash slot unavailable: {err}"))
            }),
            Err(_) => Err(CoreError::new(
                "CONSOLE_BUSY",
                "too many concurrent password calculations",
            )),
        }
    }

    /// Hash Argon2 na pool de blocking sob o limite compartilhado (mesmo
    /// teto dos logins): criação/reset de usuário não pode abrir fila
    /// ilimitada de hashes de ~19 MiB.
    async fn hash_password_limited(&self, password: &str) -> Result<String, CoreError> {
        let password = password.to_string();
        let permit = self.acquire_hash_slot().await?;
        let result = tokio::task::spawn_blocking(move || hash_password(&password))
            .await
            .map_err(|err| {
                CoreError::new("STORE_ERROR", format!("password hash task failed: {err}"))
            })?;
        drop(permit);
        result
    }
}

fn now_epoch() -> Result<u64, CoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|err| store_err(err.to_string()))
}

/// Limites de entrada do login (antes do budget e do Argon2): username
/// trimado vazio, username/senha acima do teto, senha vazia.
fn check_login_input(username: &str, password: &str) -> Result<(), String> {
    if username.is_empty()
        || username.chars().count() > USERNAME_MAX_LEN
        || password.is_empty()
        || password.chars().count() > PASSWORD_MAX_LEN
    {
        return Err("username and password are required".to_string());
    }
    Ok(())
}

fn json_array(values: &[String]) -> Result<String, CoreError> {
    serde_json::to_string(values).map_err(|err| store_err(err.to_string()))
}

/// Escopos/permissões gravados como JSON array de strings; parse corrompido
/// é STORE_ERROR (falha fechada — nunca principal parcial).
fn parse_scopes(raw: &str) -> Result<Vec<String>, CoreError> {
    let values: Vec<String> =
        serde_json::from_str(raw).map_err(|err| store_err(err.to_string()))?;
    Ok(values)
}

/// Adiciona a coluna à `console_users` apenas se ausente (migração
/// idempotente — reabrir não repete o ALTER).
fn ensure_column(
    conn: &mut Connection,
    columns: &HashSet<String>,
    name: &str,
    ddl: &str,
) -> Result<(), CoreError> {
    if !columns.contains(name) {
        conn.execute(&format!("ALTER TABLE console_users ADD COLUMN {ddl}"), [])
            .map_err(store_err)?;
    }
    Ok(())
}

fn username_taken_err() -> CoreError {
    CoreError::new("USERNAME_TAKEN", "username is already in use")
}

fn user_not_found_err(id: i64) -> CoreError {
    CoreError::new("NOT_FOUND", format!("console user {id} not found"))
}

fn user_immutable_err() -> CoreError {
    CoreError::new(
        "USER_IMMUTABLE",
        "the reserved 'root' user cannot be modified",
    )
}

/// Prova de senha/sessão obsoleta entre a verificação e a escrita (troca de
/// senha ou revogação concorrente): o banco é rejeitado SEM alteração.
fn proof_stale_err() -> CoreError {
    CoreError::new(
        "PROOF_STALE",
        "password proof or session is no longer valid",
    )
}

fn store_err<E: std::fmt::Display>(err: E) -> CoreError {
    CoreError::new("STORE_ERROR", err.to_string())
}

fn lock_err() -> CoreError {
    CoreError::new("STORE_ERROR", "sqlite connection lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use edger_core::principal_has_permission;
    use rusqlite::params as raw_params;

    const STRONG: &str = "Str0ng!Passw0rd";
    const STRONG_2: &str = "An0ther!Passw0rd";
    const OP_PASSWORD: &str = "Op3rator!Passw0rd";

    fn ip_a() -> IpAddr {
        IpAddr::from([10u8, 0, 0, 1])
    }

    fn ip_b() -> IpAddr {
        IpAddr::from([10u8, 0, 0, 2])
    }

    fn seeded() -> ConsoleAuthService {
        let service = ConsoleAuthService::in_memory().unwrap();
        service.seed_root_if_empty(Some(STRONG)).unwrap();
        service
    }

    /// Usuária adicional `alice` com duas permissões do catálogo — helper
    /// dos testes de sessão/escopo/revogação (hash direto no store: a
    /// validação de grant é coberta em `validate_user_grant`).
    fn operator_user_id(service: &ConsoleAuthService) -> i64 {
        service
            .store
            .create_user_record(
                "alice",
                &hash_password(OP_PASSWORD).unwrap(),
                &["workers:read".to_string(), "keys:manage".to_string()],
                &["*".to_string()],
                &["*".to_string()],
                1_700_000_000,
            )
            .unwrap()
            .id
    }

    fn user_by_name(service: &ConsoleAuthService, username: &str) -> ConsoleUserInfo {
        service
            .store
            .list_users()
            .unwrap()
            .into_iter()
            .find(|user| user.username == username)
            .unwrap_or_else(|| panic!("usuário {username} não encontrado"))
    }

    #[test]
    fn policy_requires_length_letter_digit_and_symbol() {
        assert!(validate_password_policy(STRONG).is_ok());
        assert!(validate_password_policy("short1!A").is_err());
        assert!(validate_password_policy("onlyletters12345").is_err());
        assert!(validate_password_policy("onlyletters!!").is_err());
        assert!(validate_password_policy("1234567890!").is_err());
        assert!(validate_password_policy(&"a".repeat(129)).is_err());
        // 128 é o teto (com dígito/símbolo).
        let at_cap = format!("a{}!{}", "a".repeat(125), 1);
        assert_eq!(at_cap.chars().count(), 128);
        assert!(validate_password_policy(&at_cap).is_ok());
    }

    #[test]
    fn load_seed_password_rejects_missing_empty_and_weak_files() {
        let dir = tempfile::tempdir().unwrap();

        assert!(load_seed_password(&dir.path().join("nope")).is_err());

        let empty = dir.path().join("empty");
        std::fs::write(&empty, "").unwrap();
        assert!(load_seed_password(&empty).is_err());

        let blank = dir.path().join("blank");
        std::fs::write(&blank, "   \n\t ").unwrap();
        assert!(load_seed_password(&blank).is_err());

        let weak = dir.path().join("weak");
        std::fs::write(&weak, "weakpassword").unwrap();
        assert!(load_seed_password(&weak).is_err());

        let strong = dir.path().join("strong");
        std::fs::write(&strong, format!("{STRONG}\n")).unwrap();
        assert_eq!(
            load_seed_password(&strong).unwrap(),
            Some(STRONG.to_string())
        );
    }

    #[test]
    fn seed_root_only_when_root_absent_and_rejects_weak() {
        let service = ConsoleAuthService::in_memory().unwrap();
        assert!(!service.seed_root_if_empty(None).unwrap());
        assert!(!service.has_root_user().unwrap());

        assert!(service.seed_root_if_empty(Some(STRONG)).unwrap());
        assert!(service.has_root_user().unwrap());
        // Semente repetida (senha diferente) NÃO reescreve o root existente.
        assert!(!service.seed_root_if_empty(Some(STRONG_2)).unwrap());
        assert!(service.login(ip_a(), "root", STRONG).is_ok());
        assert!(matches!(
            service.login(ip_a(), "root", STRONG_2).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
        // Semente fraca com root ausente é erro, não seed silenciosa.
        let fresh = ConsoleAuthService::in_memory().unwrap();
        assert!(fresh.seed_root_if_empty(Some("weak")).is_err());
        assert!(!fresh.has_root_user().unwrap());
    }

    #[test]
    fn seed_root_when_root_absent_keeps_existing_operators_and_sessions() {
        // Operador criado pelo root token ANTES de existir `EDGER_ROOT_PASSWORD_FILE`
        // (root ausente, banco não vazio): a semente posterior precisa criar o
        // root sem tocar no operador.
        let service = ConsoleAuthService::in_memory().unwrap();
        service
            .store
            .create_user_record(
                "alice",
                &hash_password(OP_PASSWORD).unwrap(),
                &["workers:read".to_string()],
                &["*".to_string()],
                &["*".to_string()],
                1_700_000_000,
            )
            .unwrap();
        let token = service.login(ip_a(), "alice", OP_PASSWORD).unwrap();
        assert!(service.authenticate_session(&token).is_some());

        // Boot com seed: cria o root e PRESERVA o operador e a sessão dele.
        assert!(service.seed_root_if_empty(Some(STRONG)).unwrap());
        assert!(service.has_root_user().unwrap());
        assert!(service.login(ip_a(), "root", STRONG).is_ok());
        assert!(service.authenticate_session(&token).is_some());
        let users = service.store.list_users().unwrap();
        assert_eq!(users.len(), 2);
        assert!(users.iter().any(|user| user.username == "alice"));

        // Segundo boot com seed DIFERENTE: não altera a senha do root.
        assert!(!service.seed_root_if_empty(Some(STRONG_2)).unwrap());
        assert!(matches!(
            service.login(ip_a(), "root", STRONG_2).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
        assert!(service.login(ip_a(), "root", STRONG).is_ok());
        // Sem semente com root presente: nada muda.
        assert!(!service.seed_root_if_empty(None).unwrap());
        assert!(service.authenticate_session(&token).is_some());
    }

    #[test]
    fn login_unknown_user_and_wrong_password_are_indistinguishable() {
        let service = seeded();
        operator_user_id(&service);

        let unknown = service.login(ip_a(), "admin", STRONG).unwrap_err();
        let wrong = service.login(ip_a(), "root", "wrong-pass-1!").unwrap_err();
        assert!(matches!(unknown, ConsoleAuthError::InvalidCredentials));
        assert!(matches!(wrong, ConsoleAuthError::InvalidCredentials));
        // `Root` (case-insensitive) autentica — normalização de login.
        assert!(service.login(ip_a(), "Root", STRONG).is_ok());
        // Usuário adicional segue a MESMA normalização case-insensitive.
        assert!(service.login(ip_a(), "ALICE", OP_PASSWORD).is_ok());
    }

    #[test]
    fn username_validation_rejects_hostile_and_reserved_names() {
        // Válidos: 2–32 chars de [a-z0-9._-], início alfanumérico.
        for name in ["ab", "alice", "a.b-c_d9", "9lives"] {
            assert!(validate_username(name).is_ok(), "deveria aceitar {name:?}");
        }
        assert!(validate_username(&format!("a{}", "b".repeat(31))).is_ok());
        // Hostil: comprimento, caixa, espaço/Unicode/controle, pontuação
        // inicial e `root` reservado (case-insensitive).
        for name in [
            "a",
            &"a".repeat(33),
            "Alice",
            "al ice",
            "al\u{00e7}ice",
            "al\nice",
            ".lead",
            "-lead",
            "_lead",
            "root",
            "ROOT",
            "RoOt",
        ] {
            assert!(
                validate_username(name).is_err(),
                "deveria rejeitar {name:?}"
            );
        }
    }

    #[test]
    fn user_grant_validation_rejects_star_permission_unknown_and_empty() {
        let star = vec!["*".to_string()];
        let read = vec!["workers:read".to_string()];
        // `"*"` como PERMISSÃO é indevido (fora do catálogo) — mesmo que o
        // criador seja root.
        let err = validate_user_grant("alice", &star, &star, &star).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        // Permissão desconhecida.
        let unknown = vec!["users:manage".to_string()];
        let err = validate_user_grant("alice", &unknown, &star, &star).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        // Listas vazias (permissões, namespaces, workers).
        assert!(validate_user_grant("alice", &[], &star, &star).is_err());
        assert!(validate_user_grant("alice", &read, &[], &star).is_err());
        assert!(validate_user_grant("alice", &read, &star, &[]).is_err());
        // Entrada vazia / com espaço nas pontas.
        assert!(validate_user_grant("alice", &read, &["".into()], &star).is_err());
        assert!(validate_user_grant("alice", &read, &[" @acme".into()], &star).is_err());
        // Válido: permissões do catálogo + escopos (incluindo `"*"` e glob
        // de sufixo como escopo de worker).
        let perms = vec!["workers:read".to_string(), "keys:manage".to_string()];
        let nss = vec!["@acme".to_string()];
        let workers = vec!["p-abc*".to_string()];
        assert!(validate_user_grant("alice", &perms, &nss, &workers).is_ok());
    }

    #[test]
    fn create_user_is_operator_only_and_never_stores_plaintext() {
        let service = seeded();
        let info = service
            .store
            .create_user_record(
                "alice",
                &hash_password(OP_PASSWORD).unwrap(),
                &["workers:read".to_string()],
                &["*".to_string()],
                &["*".to_string()],
                1_700_000_000,
            )
            .unwrap();
        assert_eq!(info.username, "alice");
        assert_eq!(info.role, "operator");
        assert!(!info.is_root);
        assert!(info.active);
        // Metadados SEM hash/senha (list_users): root + alice.
        let listed = service.store.list_users().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed
            .iter()
            .any(|user| user.is_root && user.role == "admin"));
        // O banco guarda hash lento (Argon2id), nunca a senha.
        let conn = service.store.conn.lock().unwrap();
        let hash: String = conn
            .query_row(
                "SELECT password_hash FROM console_users WHERE username = 'alice'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(!hash.contains(OP_PASSWORD));
    }

    #[test]
    fn create_user_conflict_is_case_insensitive_and_root_is_reserved() {
        let service = seeded();
        // `ALICE` (case distinto) inserido por SQL direto para controlar a
        // case gravada no banco.
        let conn = service.store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO console_users
             (username, password_hash, active, role, is_root, permissions, namespaces, workers,
              created_at, updated_at, password_changed_at)
             VALUES ('ALICE', 'x', 1, 'operator', 0, '[\"workers:read\"]', '[\"*\"]', '[\"*\"]', 1, 1, 1)",
            [],
        )
        .unwrap();
        drop(conn);
        // Unicidade case-insensitive (COLLATE NOCASE) nas duas direções:
        // `ALICE` bloqueia `alice`/`Alice` — e o erro vira o código estável
        // USERNAME_TAKEN (não um 500 de constraint).
        for username in ["alice", "Alice", "ALICE"] {
            let err = service
                .store
                .create_user_record(
                    username,
                    &hash_password(STRONG_2).unwrap(),
                    &["workers:read".to_string()],
                    &["*".to_string()],
                    &["*".to_string()],
                    1_700_000_001,
                )
                .unwrap_err();
            assert_eq!(err.code, "USERNAME_TAKEN", "{username}");
        }
        // `root` é reservado na validação (400) — nem com root ausente ele
        // entra pela API de usuários.
        let star = vec!["*".to_string()];
        let err = validate_user_grant("root", &star, &star, &star).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        let err = validate_user_grant("ROOT", &star, &star, &star).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
    }

    #[test]
    fn operator_login_session_resolves_scoped_principal_from_current_state() {
        let service = seeded();
        let id = operator_user_id(&service);
        // Login do operador (username case-insensitive).
        let token = service.login(ip_a(), "ALICE", OP_PASSWORD).unwrap();
        let principal = service.authenticate_session(&token).unwrap();
        assert!(!principal.is_root);
        assert_eq!(principal.name, "alice");
        assert_eq!(principal.role, "operator");
        assert_eq!(
            principal.permissions,
            vec!["workers:read".to_string(), "keys:manage".to_string()]
        );
        assert!(principal_has_permission(&principal, "keys:manage"));

        // Desativação: a sessão morre NA HORA (estado atual, sem cache) e o
        // login com senha certa também é negado.
        service
            .store
            .update_user_tx(
                id,
                &ConsoleUserPatch {
                    disabled: Some(true),
                    ..Default::default()
                },
                1_700_000_100,
            )
            .unwrap();
        assert!(service.authenticate_session(&token).is_none());
        assert!(matches!(
            service.login(ip_a(), "alice", OP_PASSWORD).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));

        // Reativação: volta a funcionar.
        service
            .store
            .update_user_tx(
                id,
                &ConsoleUserPatch {
                    disabled: Some(false),
                    ..Default::default()
                },
                1_700_000_101,
            )
            .unwrap();
        assert!(service.login(ip_a(), "alice", OP_PASSWORD).is_ok());
    }

    #[test]
    fn scope_reduction_revokes_sessions_and_expansion_applies_immediately() {
        let service = seeded();
        let id = operator_user_id(&service);
        let token = service.login(ip_a(), "alice", OP_PASSWORD).unwrap();
        let principal = service.authenticate_session(&token).unwrap();
        assert!(principal_has_permission(&principal, "keys:manage"));

        // REDUÇÃO: revoga a sessão na mesma transação (o principal antigo
        // nunca segue com o privilégio — com ou sem cache).
        service
            .store
            .update_user_tx(
                id,
                &ConsoleUserPatch {
                    permissions: Some(vec!["workers:read".to_string()]),
                    ..Default::default()
                },
                1_700_000_200,
            )
            .unwrap();
        assert!(service.authenticate_session(&token).is_none());

        // Sessão NOVA carrega o escopo reduzido.
        let fresh = service.login(ip_a(), "alice", OP_PASSWORD).unwrap();
        let principal = service.authenticate_session(&fresh).unwrap();
        assert!(!principal_has_permission(&principal, "keys:manage"));
        assert!(principal_has_permission(&principal, "workers:read"));

        // EXPANSÃO: não revoga — a sessão viva passa a resolver o escopo
        // maior imediatamente (estado atual do usuário).
        service
            .store
            .update_user_tx(
                id,
                &ConsoleUserPatch {
                    permissions: Some(vec!["workers:read".to_string(), "keys:manage".to_string()]),
                    ..Default::default()
                },
                1_700_000_201,
            )
            .unwrap();
        let principal = service.authenticate_session(&fresh).unwrap();
        assert!(principal_has_permission(&principal, "keys:manage"));
    }

    #[test]
    fn reset_password_revokes_sessions_and_old_password_fails() {
        let service = seeded();
        let id = operator_user_id(&service);
        let token = service.login(ip_a(), "alice", OP_PASSWORD).unwrap();
        service
            .store
            .reset_user_password_tx(id, &hash_password(STRONG_2).unwrap(), 1_700_000_300)
            .unwrap();
        // Sessão morre; senha antiga não; senha nova entra.
        assert!(service.authenticate_session(&token).is_none());
        assert!(matches!(
            service.login(ip_a(), "alice", OP_PASSWORD).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
        assert!(service.login(ip_a(), "alice", STRONG_2).is_ok());
    }

    #[test]
    fn delete_user_removes_user_and_sessions_atomically() {
        let service = seeded();
        let id = operator_user_id(&service);
        let token = service.login(ip_a(), "alice", OP_PASSWORD).unwrap();
        service.store.delete_user_tx(id).unwrap();
        assert!(service.authenticate_session(&token).is_none());
        assert!(matches!(
            service.login(ip_a(), "alice", OP_PASSWORD).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
        assert!(!service
            .store
            .list_users()
            .unwrap()
            .iter()
            .any(|user| user.username == "alice"));
        // Sem alvo: 404 honesto.
        let err = service.store.delete_user_tx(id).unwrap_err();
        assert_eq!(err.code, "NOT_FOUND");
    }

    #[test]
    fn root_user_cannot_be_updated_reset_or_deleted() {
        let service = seeded();
        let root_id = service.store.root_user().unwrap().unwrap().0;
        let err = service
            .store
            .update_user_tx(
                root_id,
                &ConsoleUserPatch {
                    disabled: Some(true),
                    ..Default::default()
                },
                1_700_000_400,
            )
            .unwrap_err();
        assert_eq!(err.code, "USER_IMMUTABLE");
        let err = service
            .store
            .reset_user_password_tx(root_id, &hash_password(STRONG_2).unwrap(), 1_700_000_401)
            .unwrap_err();
        assert_eq!(err.code, "USER_IMMUTABLE");
        assert_eq!(
            service.store.delete_user_tx(root_id).unwrap_err().code,
            "USER_IMMUTABLE"
        );
        // Root segue intacto e autenticando.
        assert!(service.login(ip_a(), "root", STRONG).is_ok());
        let session = service.login(ip_b(), "root", STRONG).unwrap();
        assert!(service.authenticate_session(&session).is_some());
    }

    #[test]
    fn update_with_invalid_final_set_fails_without_changing_db() {
        let service = seeded();
        let id = operator_user_id(&service);
        // Deixar permissões vazias no PATCH: VALIDATION_ERROR e o banco não
        // muda (o conjunto FINAL é que passa no catálogo).
        let err = service
            .store
            .update_user_tx(
                id,
                &ConsoleUserPatch {
                    permissions: Some(vec![]),
                    ..Default::default()
                },
                1_700_000_500,
            )
            .unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        let after = user_by_name(&service, "alice");
        assert_eq!(
            after.permissions,
            vec!["workers:read".to_string(), "keys:manage".to_string()]
        );
        assert!(after.active);
        // Id inexistente: 404.
        let err = service
            .store
            .update_user_tx(
                9999,
                &ConsoleUserPatch {
                    disabled: Some(true),
                    ..Default::default()
                },
                1_700_000_501,
            )
            .unwrap_err();
        assert_eq!(err.code, "NOT_FOUND");
    }

    #[test]
    fn change_password_works_for_operator_sessions() {
        let service = seeded();
        operator_user_id(&service);
        let token = service.login(ip_a(), "alice", OP_PASSWORD).unwrap();
        let fresh = service
            .change_password(ip_a(), &token, OP_PASSWORD, STRONG_2)
            .unwrap();
        assert_ne!(fresh, token);
        // Toda sessão da usuária morre; a nova vive; a senha antiga não.
        assert!(service.authenticate_session(&token).is_none());
        assert!(service.authenticate_session(&fresh).is_some());
        assert!(matches!(
            service.login(ip_a(), "alice", OP_PASSWORD).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
        // Senha atual errada: genérica (e o root segue intacto).
        assert!(matches!(
            service
                .change_password(ip_a(), &fresh, "Wrong!Pass1", STRONG)
                .unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
        assert!(service.login(ip_a(), "root", STRONG).is_ok());
    }

    #[test]
    fn injected_failure_mid_transaction_rolls_back_user_and_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("console.db");
        let service = ConsoleAuthService::open(&path).unwrap();
        service.seed_root_if_empty(Some(STRONG)).unwrap();
        let id = operator_user_id(&service);
        let token = service.login(ip_a(), "alice", OP_PASSWORD).unwrap();
        assert!(service.authenticate_session(&token).is_some());

        // Falha determinística SEM corrida: trigger `BEFORE DELETE` em
        // `console_sessions` aborta o segundo statement da transação (o
        // DELETE de sessões, que vem DEPOIS do UPDATE de `console_users` em
        // disable/redução/reset).
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_session_delete BEFORE DELETE ON console_sessions
             BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
        )
        .unwrap();
        drop(conn);

        // (a) Desativação: a transação deve FALHAR — o UPDATE aplicado é
        // desfeito e as sessões não tocam.
        let err = service
            .store
            .update_user_tx(
                id,
                &ConsoleUserPatch {
                    disabled: Some(true),
                    ..Default::default()
                },
                1_700_000_600,
            )
            .unwrap_err();
        assert_eq!(err.code, "STORE_ERROR");
        // Rollback estrutural: usuário ainda ativo com o escopo original e a
        // sessão segue viva.
        let after = user_by_name(&service, "alice");
        assert!(after.active);
        assert_eq!(
            after.permissions,
            vec!["workers:read".to_string(), "keys:manage".to_string()]
        );
        assert!(service.authenticate_session(&token).is_some());

        // (b) Reset de senha: MESMO rollback — a senha antiga segue valendo
        // (o UPDATE foi desfeito) e a sessão segue viva.
        let err = service
            .store
            .reset_user_password_tx(id, &hash_password(STRONG_2).unwrap(), 1_700_000_601)
            .unwrap_err();
        assert_eq!(err.code, "STORE_ERROR");
        assert!(service.login(ip_b(), "alice", OP_PASSWORD).is_ok());
        assert!(matches!(
            service.login(ip_b(), "alice", STRONG_2).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
        assert!(service.authenticate_session(&token).is_some());

        // Remove o trigger: a MESMA operação então conclui.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("DROP TRIGGER fail_session_delete", [])
            .unwrap();
        drop(conn);
        service
            .store
            .update_user_tx(
                id,
                &ConsoleUserPatch {
                    disabled: Some(true),
                    ..Default::default()
                },
                1_700_000_602,
            )
            .unwrap();
        let after = user_by_name(&service, "alice");
        assert!(!after.active);
        assert!(service.authenticate_session(&token).is_none());
    }

    #[test]
    fn session_token_is_opaque_32_bytes_and_only_hashed_in_db() {
        let service = seeded();
        let token = service.login(ip_a(), "root", STRONG).unwrap();

        assert!(token.starts_with(SESSION_PREFIX));
        let body = token.strip_prefix(SESSION_PREFIX).unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(body).unwrap();
        assert_eq!(bytes.len(), 32);
        // O bruto nunca chega ao banco: a linha guarda o hash com namespace
        // próprio (diferente do bruto e do hash de api-keys).
        let hash = hash_token(&token);
        assert_ne!(hash, token);
        assert!(service.authenticate_session(&token).is_some());
        // Um token `ses-` aleatório (que não existe) não autentica.
        assert!(service
            .authenticate_session("ses-0000000000000000")
            .is_none());
        // Prefixo estranho cai no lugar errado.
        assert!(service.authenticate_session("egk_nao-e-sessao").is_none());
    }

    #[test]
    fn session_issue_fails_when_password_changes_between_proof_and_issue() {
        let service = seeded();
        let (user_id, hash1) = service
            .store
            .prove_password("root", STRONG)
            .unwrap()
            .expect("prova válida com a senha certa");
        // Troca de senha CONCORRENTE entre a prova e a emissão: um writer
        // externo muda o hash (ordem de corrida determinística).
        let conn = service.store.conn.lock().unwrap();
        conn.execute(
            "UPDATE console_users SET password_hash = ?1 WHERE id = ?2",
            raw_params![hash_password(STRONG_2).unwrap(), user_id],
        )
        .unwrap();
        drop(conn);
        // A emissão reconfirma a prova e falha: nenhuma sessão nasce.
        let err = service
            .store
            .issue_session_if_proof_still_valid(user_id, &hash1, "ses-teste-123", 1, 2)
            .unwrap_err();
        assert_eq!(err.code, "PROOF_STALE");
        let sessions: i64 = service
            .store
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM console_sessions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(sessions, 0);
        // A senha antiga não emite mais sessão pelo caminho completo.
        assert!(matches!(
            service.login(ip_a(), "root", STRONG).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
    }

    #[test]
    fn change_password_rejects_when_requesting_session_was_revoked() {
        let service = seeded();
        let session = service.login(ip_a(), "root", STRONG).unwrap();
        // Logout CONCORRENTE entre a prova e o commit.
        service.logout(&session).unwrap();
        let err = service
            .change_password(ip_a(), &session, STRONG, STRONG_2)
            .unwrap_err();
        assert!(matches!(err, ConsoleAuthError::InvalidCredentials));
        // Banco intacto: a senha antiga segue valendo e a nova não entrou.
        assert!(service.login(ip_a(), "root", STRONG).is_ok());
        assert!(matches!(
            service.login(ip_a(), "root", STRONG_2).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
    }

    #[test]
    fn change_password_fails_when_hash_changes_after_proof_and_db_stays_intact() {
        let service = seeded();
        let session = service.login(ip_a(), "root", STRONG).unwrap();
        let (user_id, hash1) = service
            .store
            .prove_password("root", STRONG)
            .unwrap()
            .unwrap();
        // Writer concorrente muda o hash entre a prova e o commit.
        let hash2 = hash_password(STRONG_2).unwrap();
        let conn = service.store.conn.lock().unwrap();
        conn.execute(
            "UPDATE console_users SET password_hash = ?1 WHERE id = ?2",
            raw_params![hash2, user_id],
        )
        .unwrap();
        drop(conn);
        let err = service
            .store
            .change_password_and_reissue(
                &session,
                &hash1,
                &hash_password("New!Passw0rd3").unwrap(),
                "ses-nova-token",
                100,
            )
            .unwrap_err();
        assert_eq!(err.code, "PROOF_STALE");
        // Banco intacto: o hash continua o do writer concorrente (NÃO o
        // novo) e a sessão solicitante segue viva (não foi rotacionada).
        assert_eq!(service.store.root_user().unwrap().unwrap().1, hash2);
        assert!(service.authenticate_session(&session).is_some());
    }

    #[test]
    fn expired_session_is_purged_and_not_resurrected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("console.db");
        let token;
        {
            let service = ConsoleAuthService::open(&path).unwrap();
            service.seed_root_if_empty(Some(STRONG)).unwrap();
            token = service.login(ip_a(), "root", STRONG).unwrap();
            assert!(service.authenticate_session(&token).is_some());
            // Envelhecer a sessão além do TTL "por fora" do relógio.
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute("UPDATE console_sessions SET expires_at = 1", [])
                .unwrap();
            // Expirada: nega E remove (a linha some do banco).
            assert!(service.authenticate_session(&token).is_none());
            let rows: i64 = conn
                .query_row("SELECT COUNT(*) FROM console_sessions", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(rows, 0);
        }
        // Restart: a sessão morta não ressuscita.
        let reopened = ConsoleAuthService::open(&path).unwrap();
        assert!(reopened.authenticate_session(&token).is_none());
    }

    #[test]
    fn logout_is_terminal_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("console.db");
        let token;
        {
            let service = ConsoleAuthService::open(&path).unwrap();
            service.seed_root_if_empty(Some(STRONG)).unwrap();
            token = service.login(ip_a(), "root", STRONG).unwrap();
            assert!(service.authenticate_session(&token).is_some());
            assert!(service.logout(&token).unwrap());
            assert!(service.authenticate_session(&token).is_none());
            // Revogar de novo é no-op honesto.
            assert!(!service.logout(&token).unwrap());
        }
        let reopened = ConsoleAuthService::open(&path).unwrap();
        assert!(reopened.authenticate_session(&token).is_none());
    }

    #[test]
    fn change_password_rotates_sessions_and_validates_both_sides() {
        let service = seeded();
        let old = service.login(ip_a(), "root", STRONG).unwrap();
        let second = service.login(ip_a(), "root", STRONG).unwrap();
        assert_ne!(old, second);

        // Senha atual errada: genérica (e consome budget).
        assert!(matches!(
            service
                .change_password(ip_a(), &old, "wrong-pass-1!", STRONG_2)
                .unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
        // Nova senha fraca: 400 antes de tocar no banco.
        assert!(matches!(
            service
                .change_password(ip_a(), &old, STRONG, "weak")
                .unwrap_err(),
            ConsoleAuthError::InvalidRequest(_)
        ));

        let fresh = service
            .change_password(ip_a(), &old, STRONG, STRONG_2)
            .unwrap();
        // As antigas (todas) morrem; a nova autentica; a senha antiga não.
        assert!(service.authenticate_session(&old).is_none());
        assert!(service.authenticate_session(&second).is_none());
        assert!(service.authenticate_session(&fresh).is_some());
        assert!(matches!(
            service.login(ip_a(), "root", STRONG).unwrap_err(),
            ConsoleAuthError::InvalidCredentials
        ));
        assert!(service.login(ip_a(), "root", STRONG_2).is_ok());
    }

    #[test]
    fn legacy_2601_schema_migrates_without_losing_root_or_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("console.db");

        // 1) Banco no schema EXATO da fatia root (26.01): 6 colunas, root
        //    semeado e uma sessão viva criada pelo próprio código antigo
        //    (schema antigo aceita INSERT direto com as colunas da época).
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE console_users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE COLLATE NOCASE,
                password_hash TEXT NOT NULL,
                active INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                password_changed_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );
            CREATE TABLE console_sessions (
                id TEXT PRIMARY KEY,
                user_id INTEGER NOT NULL REFERENCES console_users(id),
                token_hash TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );",
        )
        .unwrap();
        let hash = hash_password(STRONG).unwrap();
        conn.execute(
            "INSERT INTO console_users (username, password_hash, active, created_at, password_changed_at)
             VALUES ('root', ?1, 1, 1_700_000_000, 1_700_000_000)",
            raw_params![hash],
        )
        .unwrap();
        let token = format!("{SESSION_PREFIX}migracao-teste-0123456789abcdef");
        conn.execute(
            "INSERT INTO console_sessions (id, user_id, token_hash, created_at, expires_at)
             VALUES ('migrated-session', 1, ?1, 1_700_000_000, 9_999_999_999)",
            raw_params![hash_token(&token)],
        )
        .unwrap();
        drop(conn);

        // 2) Abertura com o código novo: migração idempotente (ALTER +
        //    defaults seguros), sem recriar tabela nem perder root/sessões.
        let service = ConsoleAuthService::open(&path).unwrap();
        assert!(service.has_root_user().unwrap());
        // Root segue autenticando com a senha original, e a sessão viva da
        // época segue resolviendo para o principal de root.
        assert!(service.login(ip_a(), "root", STRONG).is_ok());
        let principal = service.authenticate_session(&token).unwrap();
        assert!(principal.is_root);
        assert_eq!(principal.name, "root");
        // Metadados do root após a migração: role admin, is_root, `"*"`.
        let root = user_by_name(&service, "root");
        assert!(root.is_root);
        assert_eq!(root.role, "admin");
        assert_eq!(root.permissions, vec!["*".to_string()]);
        assert!(root.active);

        // 3) Novo usuário pode ser criado no banco migrado.
        let info = service
            .store
            .create_user_record(
                "alice",
                &hash_password(OP_PASSWORD).unwrap(),
                &["workers:read".to_string()],
                &["*".to_string()],
                &["*".to_string()],
                1_700_001_000,
            )
            .unwrap();
        assert!(!info.is_root);
        assert_eq!(info.role, "operator");
        assert!(service.login(ip_a(), "alice", OP_PASSWORD).is_ok());

        // 4) Reabertura NÃO repete nem estoura a migração (idempotente):
        //    colunas únicas, dados intactos, sessão e root válidos.
        let columns: Vec<String> = {
            let conn = rusqlite::Connection::open(&path).unwrap();
            let mut stmt = conn.prepare("PRAGMA table_info(console_users)").unwrap();
            stmt.query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let expected = [
            "id",
            "username",
            "password_hash",
            "active",
            "role",
            "is_root",
            "permissions",
            "namespaces",
            "workers",
            "created_at",
            "updated_at",
            "password_changed_at",
        ];
        for column in expected {
            assert!(columns.iter().any(|c| c == column), "faltando {column}");
        }
        assert_eq!(
            columns.len(),
            expected.len(),
            "coluna duplicada? {columns:?}"
        );
        let reopened = ConsoleAuthService::open(&path).unwrap();
        assert!(reopened.authenticate_session(&token).is_some());
        assert!(reopened.login(ip_a(), "root", STRONG).is_ok());
        assert!(reopened.login(ip_a(), "alice", OP_PASSWORD).is_ok());
        let root = user_by_name(&reopened, "root");
        assert_eq!(root.permissions, vec!["*".to_string()]);
        // NENHUM outro registro foi promovido a root/admin.
        let alice = user_by_name(&reopened, "alice");
        assert!(!alice.is_root);
        assert_eq!(alice.role, "operator");
    }

    #[test]
    fn limiter_is_per_real_ip_with_retry_after_and_sliding_window() {
        // Janela longa de propósito: cada login errado roda Argon2 real
        // (centenas de ms), e a janela de 1s do rascunho tornava o teste
        // flaky sob carga (as falhas deslizavam da janela antes da 4a checagem).
        let service =
            ConsoleAuthService::in_memory_with_limiter(Duration::from_secs(60), 3).unwrap();
        service.seed_root_if_empty(Some(STRONG)).unwrap();

        for _ in 0..3 {
            assert!(matches!(
                service.login(ip_a(), "root", "wrong-pass-1!").unwrap_err(),
                ConsoleAuthError::InvalidCredentials
            ));
        }
        // IP A esgotado: até senha CORreta vira 429 (falha fechado por IP).
        match service.login(ip_a(), "root", STRONG) {
            Err(ConsoleAuthError::RateLimited(retry)) => {
                // Retry-After: mínimo de 1s, nunca acima da janela.
                assert!(retry >= Duration::from_secs(1));
                assert!(retry <= Duration::from_secs(60));
            }
            other => panic!("esperava RateLimited, veio {other:?}"),
        }
        // IP B segue livre: o budget é por IP real, não global.
        assert!(service.login(ip_b(), "root", STRONG).is_ok());
    }

    #[test]
    fn limiter_window_slides_and_releases_budget() {
        // Limiter puro (sem Argon2): o deslizamento da janela é determinístico.
        let limiter = LoginLimiter::new(Duration::from_secs(1), 3);
        let ip = IpAddr::from([10, 0, 0, 9]);
        for _ in 0..3 {
            limiter.record_failure(ip);
        }
        assert!(matches!(
            limiter.check(ip),
            Some(retry) if retry <= Duration::from_secs(1)
        ));
        // IP distinto continua livre.
        assert!(limiter.check(IpAddr::from([10, 0, 0, 10])).is_none());
        // A janela desliza: depois de 1,1s o budget limpa.
        std::thread::sleep(Duration::from_millis(1_100));
        assert!(limiter.check(ip).is_none());
    }

    #[test]
    fn limiter_sweep_keeps_bucket_memory_bounded() {
        let limiter = LoginLimiter::new(Duration::from_secs(3_600), 5);
        // Estoura o teto de buckets com IPs distintos (sem argon2: barato).
        for i in 0..=MAX_LIMITER_BUCKETS {
            let ip = IpAddr::from([10, 0, (i / 256) as u8, (i % 256) as u8]);
            limiter.record_failure(ip);
        }
        let buckets = limiter.lock();
        assert!(buckets.len() <= MAX_LIMITER_BUCKETS);
        drop(buckets);
        // `check` nunca cria bucket: IP que nunca falhou não vira entrada.
        let before = limiter.lock().len();
        limiter.check(IpAddr::from([127, 0, 0, 1]));
        assert_eq!(limiter.lock().len(), before);
    }
}
