//! Registro host durável de identidades e reservas do provisionador sqld
//! (EDG-54 / 13a).
//!
//! Ledger LOCAL em SQLite que guarda, por dono (`worker` + namespace RBAC
//! opcional), (a) o namespace SQL derivado, (b) a chave pública Ed25519 do app,
//! (c) o envelope ENCRIPTADO da chave privada (via [`crate::sqld_signer`], sob
//! uma master dedicada fornecida pelo chamador), (d) a reserva de quota e
//! (e) o estado de prova de posse. Não faz rede, HTTP, env, configuração nem
//! log. Não é o provisionador nem a Admin API, e NÃO prova nada sobre o
//! SQLDengine remoto.
//!
//! # O que o registro garante (e o que não)
//!
//! * Identidade: `UNIQUE(worker, rbac_tag, rbac_namespace)`; `None` e
//!   `Some(_)` são tags diferentes (`rbac_tag` 0/1, sem NULL), então não há
//!   brecha de unicidade com NULL. O nome do worker é a identidade íntegra
//!   (sem trim/normalização). O namespace SQL é determinístico:
//!   `edger_` + hex(SHA-256(`"edger.sqld-registry.ns.v1\0"` || u32be(len
//!   worker) || worker || tag || [u32be(len ns) || ns])`; sem geração, versão
//!   de deploy nem process id. Uma colisão vira [`SqldRegistryError::
//!   NamespaceCollision`]: nunca sufixo aleatório nem reuso de namespace alheio.
//! * Chave: gerada uma vez, aleatória, selada com a AAD do signer
//!   (`worker`, `rbac_namespace`, geração 1). A linha é gravada na MESMA
//!   transação (`BEGIN IMMEDIATE`) que a reserva de quota, antes de a chamada
//!   retornar. Chamadas repetidas com a mesma quota devolvem a mesma
//!   identidade (sem regenerar); quota diferente é conflito. Nenhum tipo
//!   público expõe chave privada, PKCS#8 ou envelope; só o JWT escopado sai
//!   (via `SqldScopedToken`, `Debug` redigido).
//! * Prova de posse: `Intent` (metadata ou um `CREATE` com 400 NÃO provam
//!   posse), `Proven`, `Ready`, `Conflict`. [`SqldProvisionRegistry::
//!   mark_proven`] só compara (tempo constante) a chave pública observada com
//!   a gravada: ele NÃO faz GET nem sabe de onde `observed` veio. O chamador
//!   host (EDG-54 / 14) deve validar um GET remoto FRESCO antes de chamar.
//!   Divergência grava `Conflict` de forma persistente e devolve erro;
//!   nunca sobrescreve nem recria a chave. `Conflict` nega `mark_*` e token.
//! * Token: [`SqldProvisionRegistry::sign_scoped_token`] só assina para
//!   `Proven`/`Ready`, e SOMENTE para o namespace SQL vinculado ao dono (o
//!   namespace não é parâmetro).
//! * Master autenticada ANTES de tudo: os metadados guardam um envelope
//!   "master check" (dono constante do host, geração 1, selado pelo signer
//!   existente sob a master; AAD do signer inalterada). `open` o abre dentro
//!   da transação de init e `with_tx` o reabre antes de qualquer
//!   leitura/escrita de dono. Master errada (também em ledger sem apps) ou
//!   check adulterado: `Integrity`, sem abrir handle, sem escrever, sem
//!   gerar identidade nova nem consumir quota. Ledger de versão de schema
//!   diferente (inclusive o v1 sem check, nunca integrado) é recusado com
//!   `UnsupportedSchema`; não há migração.
//! * Integridade na leitura: toda operação por dono abre o envelope com
//!   master + dono + geração e compara a chave pública real com a gravada,
//!   além de recalcular o namespace SQL, antes de devolver identidade ou
//!   assinar. Master errada, envelope/nonce/dono/chave/namespace/geração
//!   adulterados dão erro estático (fail-closed), sem token e sem reparo.
//!   LIMITE: `state` e `quota_bytes` NÃO são autenticados pelo AEAD (a AAD do
//!   signer é fixa); quem escreve no arquivo pode alterá-los. A fronteira de
//!   confiança é o mesmo UID do host (ver Arquivo).
//! * Orçamento: `managed_budget_bytes` é configuração SOFT do ledger,
//!   persistida atomicamente nos metadados na criação; um `open` posterior
//!   com orçamento diferente é recusado (dois processos não obtêm orçamento
//!   maior). A admissão soma as quotas de TODAS as linhas (inclusive
//!   `Intent`/`Conflict`), com aritmética checada, e insere na mesma
//!   transação `IMMEDIATE`; duas conexões (threads ou processos) disputando a
//!   última reserva serializam no lock de escrita do SQLite. Não há API de
//!   delete/drop/liberação/reconfiguração. As reservas contam mesmo após
//!   falha. Nenhuma promessa de limite duro de PVC/WAL/backup nem de apps não
//!   gerenciados; a política 3 GiB/5 GiB é decisão futura do host.
//! * Concorrência: só há transações curtas (nada de lock mantido durante
//!   `await`). Este ledger garante quota e dono únicos; NÃO serializa RPC
//!   remoto, leases nem falhas de processo no meio de um `CREATE` (isso é do
//!   passo 14). Sem flock, daemon, thread ou timer.
//! * Locks POSIX (Linux): `close()` de qualquer fd do arquivo cancela os
//!   locks que OUTRA conexão SQLite do mesmo processo segura. Um mutex
//!   GLOBAL do processo (`fd_gate`, ordem gate -> conexão) cobre o
//!   `open`/`close` bruto do preflight, a abertura do SQLite, toda
//!   transação e o `Drop`/erro de cada `Connection`, então o preflight de
//!   um handle espera a transação do outro. Só coordena handles DESTE
//!   módulo no processo; outros processos contam com os locks do SQLite.
//! * Persistente vs memória: `open` aceita só `journal_mode = delete`; o
//!   nome `:memory:` e nomes `file:...` (URI do SQLite) são recusados antes de
//!   criar arquivo (relativo ou absoluto). `memory` só vale em `in_memory`.
//!
//! # Arquivo (Unix)
//!
//! O chamador fornece um diretório PRIVADO já existente (`0700`, dono =
//! euid, não-symlink); este módulo nunca cria nem altera pais, nem faz
//! `chmod` em pasta existente. Ancestrais acima do diretório são config
//! confiável do host. O DB é criado com `O_NOFOLLOW|O_CREAT|O_EXCL` `0600`
//! relativo ao fd do diretório e reaberto sem seguir symlink; arquivo regular,
//! dono = euid, modo exato `0600`, `nlink == 1` (senão recusa, sem tocar o
//! destino). `-journal` existente passa pelas mesmas checagens; `-wal`/`-shm`
//! presentes são recusados. `journal_mode` deve ser `delete` (nunca WAL) e
//! `synchronous=FULL`. O SQLite abre por caminho: entre a checagem e a
//! abertura há uma janela, mitigada por `O_NOFOLLOW` no SQLite e pela
//! comparação dev/inode do caminho com o fd checado; um ator com o mesmo UID
//! (ou root) malicioso está FORA do modelo. Fora de Unix, `open` devolve
//! `Unsupported`.
//!
//! O arquivo guarda apenas o envelope da chave (nunca PKCS#8 em claro nem
//! JWT). Cópias internas de ring/rusqlite não têm apagamento garantido.
//!
//! # Dependências novas
//!
//! `rustix::process::geteuid` exige a feature `process` do `rustix` (já na
//! árvore em 1.1.4; só adiciona a feature, sem nova versão). `sha2` e
//! `rusqlite` já são dependências do crate.

use std::fmt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::sqld_signer::{
    SealedSqldSigningKey, SqldScopedToken, SqldSignerError, SqldSignerOwner, SqldSigningKey,
    MASTER_KEY_LEN, MAX_NAME_LEN, PUBLIC_KEY_LEN,
};

/// Reserva mínima por app e orçamento mínimo (16 MiB).
pub const MIN_QUOTA_BYTES: u64 = 16 * 1024 * 1024;
/// Alinhamento obrigatório de quota e orçamento (página de 4 KiB).
pub const QUOTA_ALIGN_BYTES: u64 = 4096;
/// Geração única desta versão do ledger (o signer liga a geração na AAD).
pub const REGISTRY_GENERATION: u64 = 1;
/// Versão do schema próprio do ledger.
pub const REGISTRY_SCHEMA_VERSION: i64 = 2;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// `PRAGMA application_id` do ledger ("EDSQ").
const APPLICATION_ID: i64 = 0x4544_5351;
const NS_PREFIX: &[u8] = b"edger.sqld-registry.ns.v1\0";

/// Dono constante (domínio do host, geração 1) do "master check": um envelope
/// selado sob a master na criação do ledger e aberto em todo `open` e em toda
/// transação, ANTES de qualquer leitura de dono ou escrita. Vive só nos
/// metadados: não é linha de `sqld_registry`, não reserva quota nem conta no
/// orçamento. O AAD do signer não muda.
const MASTER_CHECK_WORKER: &str = "edger.sqld-registry.master-check.v1";

const META_SQL: &str = "CREATE TABLE sqld_registry_meta (\
 id INTEGER PRIMARY KEY CHECK (id = 1),\
 schema_version INTEGER NOT NULL,\
 managed_budget_bytes INTEGER NOT NULL CHECK (managed_budget_bytes > 0),\
 check_public_key BLOB NOT NULL,\
 check_nonce BLOB NOT NULL,\
 check_ciphertext BLOB NOT NULL)";

const REGISTRY_SQL: &str = "CREATE TABLE sqld_registry (\
 id INTEGER PRIMARY KEY,\
 worker TEXT NOT NULL,\
 rbac_tag INTEGER NOT NULL CHECK (rbac_tag IN (0, 1)),\
 rbac_namespace TEXT NOT NULL,\
 sql_namespace TEXT NOT NULL UNIQUE,\
 state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 3),\
 generation INTEGER NOT NULL,\
 public_key BLOB NOT NULL,\
 key_nonce BLOB NOT NULL,\
 key_ciphertext BLOB NOT NULL,\
 quota_bytes INTEGER NOT NULL CHECK (quota_bytes > 0),\
 CHECK ((rbac_tag = 0 AND rbac_namespace = '') OR (rbac_tag = 1 AND rbac_namespace <> '')),\
 UNIQUE (worker, rbac_tag, rbac_namespace))";

/// Erro estático: nunca carrega nome, caminho, chave, token nem texto SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldRegistryError {
    InvalidWorker,
    InvalidRbacNamespace,
    InvalidBudget,
    InvalidQuota,
    /// Quota diferente da já reservada para o dono.
    QuotaConflict,
    /// A soma das reservas passaria do orçamento do ledger.
    BudgetExceeded,
    /// O orçamento pedido difere do persistido no ledger.
    BudgetMismatch,
    /// O namespace SQL derivado já pertence a outro registro.
    NamespaceCollision,
    NotFound,
    /// Transição de estado não permitida (ex.: `Ready` a partir de `Intent`).
    InvalidTransition,
    /// Token pedido para dono sem posse provada (`Intent`).
    NotProven,
    /// A chave pública observada não bate com a gravada (agora `Conflict`).
    PublicKeyMismatch,
    /// O registro já está em `Conflict`.
    Conflict,
    /// Master errada ou metadata/envelope/chave/namespace adulterados.
    Integrity,
    /// Versão de schema desconhecida.
    UnsupportedSchema,
    /// Arquivo que não é (ou não é mais) um ledger válido.
    ForeignDatabase,
    /// Caminho/arquivo inseguro (symlink, hardlink, tipo, `-wal`, ausente).
    UnsafePath,
    /// Dono ou permissões do diretório/arquivo diferentes do exigido.
    WeakPermissions,
    /// `journal_mode` diferente de `delete`.
    UnsafeJournalMode,
    /// Lock de escrita não obtido dentro do busy timeout.
    Busy,
    Storage,
    /// Plataforma sem suporte a arquivo seguro.
    Unsupported,
    Signer(SqldSignerError),
}

impl fmt::Display for SqldRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidWorker => "sqld registry: invalid worker name",
            Self::InvalidRbacNamespace => "sqld registry: invalid rbac namespace",
            Self::InvalidBudget => "sqld registry: invalid managed budget",
            Self::InvalidQuota => "sqld registry: invalid quota",
            Self::QuotaConflict => "sqld registry: quota differs from the reserved one",
            Self::BudgetExceeded => "sqld registry: managed budget exceeded",
            Self::BudgetMismatch => "sqld registry: budget differs from the persisted one",
            Self::NamespaceCollision => "sqld registry: sql namespace already owned",
            Self::NotFound => "sqld registry: owner not registered",
            Self::InvalidTransition => "sqld registry: invalid state transition",
            Self::NotProven => "sqld registry: key ownership not proven",
            Self::PublicKeyMismatch => "sqld registry: observed public key mismatch",
            Self::Conflict => "sqld registry: record is in conflict",
            Self::Integrity => "sqld registry: integrity check failed",
            Self::UnsupportedSchema => "sqld registry: unsupported schema version",
            Self::ForeignDatabase => "sqld registry: not a valid registry database",
            Self::UnsafePath => "sqld registry: unsafe database path",
            Self::WeakPermissions => "sqld registry: weak owner or permissions",
            Self::UnsafeJournalMode => "sqld registry: journal mode must be delete",
            Self::Busy => "sqld registry: database busy",
            Self::Storage => "sqld registry: storage error",
            Self::Unsupported => "sqld registry: unsupported platform",
            Self::Signer(_) => "sqld registry: signer error",
        })
    }
}

impl std::error::Error for SqldRegistryError {}

impl From<SqldSignerError> for SqldRegistryError {
    fn from(value: SqldSignerError) -> Self {
        Self::Signer(value)
    }
}

fn storage(err: rusqlite::Error) -> SqldRegistryError {
    use rusqlite::ErrorCode;
    match err.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => SqldRegistryError::Busy,
        Some(ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) => {
            SqldRegistryError::ForeignDatabase
        }
        _ => SqldRegistryError::Storage,
    }
}

/// Dono confiável: identidade `WorkerRef.name` íntegra + `WorkerRef.namespace`
/// (`None` e `Some` são identidades diferentes). A geração é fixa em 1.
#[derive(Clone, Copy)]
pub struct SqldRegistryOwner<'a> {
    pub worker: &'a str,
    pub rbac_namespace: Option<&'a str>,
}

impl fmt::Debug for SqldRegistryOwner<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldRegistryOwner").finish_non_exhaustive()
    }
}

/// Estado de prova de posse da chave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqldRegistryState {
    /// Só intenção: não prova posse remota.
    Intent,
    /// Chave pública observada (validada pelo chamador) bate com a gravada.
    Proven,
    Ready,
    /// Chave pública divergente: terminal, nunca assina.
    Conflict,
}

impl SqldRegistryState {
    fn to_db(self) -> i64 {
        match self {
            Self::Intent => 0,
            Self::Proven => 1,
            Self::Ready => 2,
            Self::Conflict => 3,
        }
    }

    fn from_db(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::Intent),
            1 => Some(Self::Proven),
            2 => Some(Self::Ready),
            3 => Some(Self::Conflict),
            _ => None,
        }
    }
}

/// Registro só com metadata: sem chave privada nem envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqldRegistryRecord {
    pub state: SqldRegistryState,
    pub sql_namespace: String,
    pub public_key: [u8; PUBLIC_KEY_LEN],
    pub quota_bytes: u64,
    pub generation: u64,
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NAME_LEN && !value.chars().any(char::is_control)
}

fn validate_owner(owner: &SqldRegistryOwner<'_>) -> Result<(), SqldRegistryError> {
    if !valid_text(owner.worker) {
        return Err(SqldRegistryError::InvalidWorker);
    }
    if let Some(ns) = owner.rbac_namespace {
        if !valid_text(ns) {
            return Err(SqldRegistryError::InvalidRbacNamespace);
        }
    }
    Ok(())
}

fn validate_budget(budget: u64) -> Result<(), SqldRegistryError> {
    if budget < MIN_QUOTA_BYTES
        || i64::try_from(budget).is_err()
        || !budget.is_multiple_of(QUOTA_ALIGN_BYTES)
    {
        return Err(SqldRegistryError::InvalidBudget);
    }
    Ok(())
}

fn validate_quota(quota: u64, budget: u64) -> Result<(), SqldRegistryError> {
    if quota < MIN_QUOTA_BYTES || !quota.is_multiple_of(QUOTA_ALIGN_BYTES) || quota > budget {
        return Err(SqldRegistryError::InvalidQuota);
    }
    Ok(())
}

/// Namespace SQL determinístico (ver a documentação do módulo).
fn derive_sql_namespace(owner: &SqldRegistryOwner<'_>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(NS_PREFIX);
    // `validate_owner` limita a MAX_NAME_LEN, então o cast não trunca.
    hasher.update((owner.worker.len() as u32).to_be_bytes());
    hasher.update(owner.worker.as_bytes());
    match owner.rbac_namespace {
        None => hasher.update([0u8]),
        Some(ns) => {
            hasher.update([1u8]);
            hasher.update((ns.len() as u32).to_be_bytes());
            hasher.update(ns.as_bytes());
        }
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(6 + digest.len() * 2);
    out.push_str("edger_");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn signer_owner<'a>(owner: &SqldRegistryOwner<'a>) -> SqldSignerOwner<'a> {
    SqldSignerOwner {
        worker: owner.worker,
        rbac_namespace: owner.rbac_namespace,
        generation: REGISTRY_GENERATION,
    }
}

fn ct_eq(a: &[u8; PUBLIC_KEY_LEN], b: &[u8; PUBLIC_KEY_LEN]) -> bool {
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn owner_key<'a>(owner: &SqldRegistryOwner<'a>) -> (i64, &'a str) {
    match owner.rbac_namespace {
        None => (0, ""),
        Some(ns) => (1, ns),
    }
}

struct RawRow {
    id: i64,
    state: i64,
    generation: i64,
    sql_namespace: String,
    public_key: Vec<u8>,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    quota_bytes: i64,
}

/// Linha já verificada (envelope aberto, chave pública conferida).
struct Verified {
    id: i64,
    record: SqldRegistryRecord,
    key: SqldSigningKey,
}

const SELECT_ROW: &str = "SELECT id, state, generation, sql_namespace, public_key, key_nonce, \
 key_ciphertext, quota_bytes FROM sqld_registry \
 WHERE worker = ?1 AND rbac_tag = ?2 AND rbac_namespace = ?3";

/// Ledger durável. `Debug` não mostra a master nem o conteúdo.
pub struct SqldProvisionRegistry {
    /// `Option` para que o `Drop` feche a conexão DENTRO do gate global.
    conn: Mutex<Option<Connection>>,
    master: Zeroizing<[u8; MASTER_KEY_LEN]>,
    budget: u64,
}

impl fmt::Debug for SqldProvisionRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqldProvisionRegistry")
            .field("managed_budget_bytes", &self.budget)
            .finish_non_exhaustive()
    }
}

impl SqldProvisionRegistry {
    /// Abre (ou cria) o ledger persistente em `path`. Ver "Arquivo" na
    /// documentação do módulo: o diretório pai deve existir, `0700` e do euid.
    pub fn open(
        path: &Path,
        dedicated_master: Zeroizing<[u8; MASTER_KEY_LEN]>,
        managed_budget_bytes: u64,
    ) -> Result<Self, SqldRegistryError> {
        validate_budget(managed_budget_bytes)?;
        // Gate global do começo ao fim: o `open` bruto + `close` do
        // preflight, a abertura do SQLite e todo drop de `Connection` dos
        // caminhos de erro ficam dentro dele (ver `fd_gate`).
        let _gate = fd_gate();
        let (conn, may_init) = secure_file::open_connection(path)?;
        Self::finish_open(conn, may_init, true, dedicated_master, managed_budget_bytes)
    }

    /// Ledger em memória, para testes unitários (sem arquivo).
    pub fn in_memory(
        dedicated_master: Zeroizing<[u8; MASTER_KEY_LEN]>,
        managed_budget_bytes: u64,
    ) -> Result<Self, SqldRegistryError> {
        validate_budget(managed_budget_bytes)?;
        let _gate = fd_gate();
        let conn = Connection::open_in_memory().map_err(storage)?;
        Self::finish_open(conn, true, false, dedicated_master, managed_budget_bytes)
    }

    /// Chamado com o gate global já tomado. `persistent` = veio de `open`
    /// (arquivo): só `journal_mode = delete` vale; `memory` só no `in_memory`.
    fn finish_open(
        mut conn: Connection,
        may_init: bool,
        persistent: bool,
        master: Zeroizing<[u8; MASTER_KEY_LEN]>,
        budget: u64,
    ) -> Result<Self, SqldRegistryError> {
        conn.busy_timeout(BUSY_TIMEOUT).map_err(storage)?;
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(storage)?;
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(storage)?;
        // Em memória o SQLite reporta `memory`; arquivo só vale `delete` (um
        // caminho que o SQLite leu como `:memory:` reporta `memory` e é
        // recusado aqui).
        let accepted = if persistent { "delete" } else { "memory" };
        if mode != accepted {
            return Err(SqldRegistryError::UnsafeJournalMode);
        }
        {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            init_or_verify(&tx, may_init, budget, &master)?;
            tx.commit().map_err(storage)?;
        }
        Ok(Self {
            conn: Mutex::new(Some(conn)),
            master,
            budget,
        })
    }

    /// Só testes: lê a conexão sob o gate (ordem gate -> conexão).
    #[cfg(test)]
    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        let _gate = fd_gate();
        let guard = self.conn.lock().unwrap();
        f(guard.as_ref().unwrap())
    }

    /// Roda `f` numa transação `BEGIN IMMEDIATE` curta; só confirma se `f`
    /// devolve `Ok`. Ordem de locks fixa: gate global -> conexão. O gate fica
    /// preso durante toda a transação (e o drop dela, inclusive em erro),
    /// então nenhum `open`/`close` bruto de outro handle do processo roda
    /// enquanto este handle segura um lock POSIX do SQLite. Antes de `f` (e
    /// de qualquer leitura/escrita de dono) a master é autenticada.
    fn with_tx<T>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, SqldRegistryError>,
    ) -> Result<T, SqldRegistryError> {
        let _gate = fd_gate();
        let mut guard = self.conn.lock().map_err(|_| SqldRegistryError::Storage)?;
        let conn = guard.as_mut().ok_or(SqldRegistryError::Storage)?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        check_budget(&tx, self.budget)?;
        verify_master_check(&tx, &self.master)?;
        let value = f(&tx)?;
        tx.commit().map_err(storage)?;
        Ok(value)
    }

    fn find(
        &self,
        tx: &Transaction<'_>,
        owner: &SqldRegistryOwner<'_>,
    ) -> Result<Option<Verified>, SqldRegistryError> {
        let (tag, ns) = owner_key(owner);
        let raw = tx
            .query_row(SELECT_ROW, params![owner.worker, tag, ns], |row| {
                Ok(RawRow {
                    id: row.get(0)?,
                    state: row.get(1)?,
                    generation: row.get(2)?,
                    sql_namespace: row.get(3)?,
                    public_key: row.get(4)?,
                    nonce: row.get(5)?,
                    ciphertext: row.get(6)?,
                    quota_bytes: row.get(7)?,
                })
            })
            .optional()
            .map_err(|err| match err {
                // coluna com tipo/faixa errados: linha adulterada, não falha de I/O
                rusqlite::Error::InvalidColumnType(..)
                | rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..) => SqldRegistryError::Integrity,
                other => storage(other),
            })?;
        raw.map(|raw| self.verify(raw, owner)).transpose()
    }

    /// Abre o envelope e confere chave pública, namespace, geração, estado e
    /// quota antes de devolver qualquer identidade.
    fn verify(
        &self,
        raw: RawRow,
        owner: &SqldRegistryOwner<'_>,
    ) -> Result<Verified, SqldRegistryError> {
        let integrity = SqldRegistryError::Integrity;
        let state = SqldRegistryState::from_db(raw.state).ok_or(integrity)?;
        if u64::try_from(raw.generation).ok() != Some(REGISTRY_GENERATION) {
            return Err(integrity);
        }
        let quota = u64::try_from(raw.quota_bytes).map_err(|_| integrity)?;
        validate_quota(quota, self.budget).map_err(|_| integrity)?;
        if raw.sql_namespace != derive_sql_namespace(owner) {
            return Err(integrity);
        }
        let stored_pub: [u8; PUBLIC_KEY_LEN] = raw
            .public_key
            .as_slice()
            .try_into()
            .map_err(|_| integrity)?;
        let sealed =
            SealedSqldSigningKey::from_parts(&raw.nonce, &raw.ciphertext).map_err(|_| integrity)?;
        let key = SqldSigningKey::open(&sealed, &self.master, &signer_owner(owner))
            .map_err(|_| integrity)?;
        if !ct_eq(key.public_key(), &stored_pub) {
            return Err(integrity);
        }
        Ok(Verified {
            id: raw.id,
            record: SqldRegistryRecord {
                state,
                sql_namespace: raw.sql_namespace,
                public_key: stored_pub,
                quota_bytes: quota,
                generation: REGISTRY_GENERATION,
            },
            key,
        })
    }

    /// Garante a intenção do dono: reserva `quota_bytes` e grava a chave
    /// (selada) na mesma transação, ANTES de retornar. Idempotente para a
    /// mesma quota; quota diferente é [`SqldRegistryError::QuotaConflict`].
    /// Não prova posse remota.
    pub fn ensure_intent(
        &self,
        owner: &SqldRegistryOwner<'_>,
        quota_bytes: u64,
    ) -> Result<SqldRegistryRecord, SqldRegistryError> {
        validate_owner(owner)?;
        validate_quota(quota_bytes, self.budget)?;
        self.with_tx(|tx| {
            if let Some(found) = self.find(tx, owner)? {
                if found.record.quota_bytes != quota_bytes {
                    return Err(SqldRegistryError::QuotaConflict);
                }
                return Ok(found.record);
            }
            let sql_namespace = derive_sql_namespace(owner);
            let taken: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqld_registry WHERE sql_namespace = ?1)",
                    params![sql_namespace],
                    |row| row.get(0),
                )
                .map_err(storage)?;
            if taken {
                return Err(SqldRegistryError::NamespaceCollision);
            }
            let reserved = sum_quotas(tx)?;
            let total = reserved
                .checked_add(quota_bytes)
                .filter(|total| *total <= self.budget)
                .ok_or(SqldRegistryError::BudgetExceeded)?;
            debug_assert!(total <= self.budget);
            let key = SqldSigningKey::generate()?;
            let sealed = key.seal(&self.master, &signer_owner(owner))?;
            let (tag, ns) = owner_key(owner);
            let quota_db =
                i64::try_from(quota_bytes).map_err(|_| SqldRegistryError::InvalidQuota)?;
            tx.execute(
                "INSERT INTO sqld_registry (worker, rbac_tag, rbac_namespace, sql_namespace, \
                 state, generation, public_key, key_nonce, key_ciphertext, quota_bytes) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    owner.worker,
                    tag,
                    ns,
                    sql_namespace,
                    SqldRegistryState::Intent.to_db(),
                    REGISTRY_GENERATION as i64,
                    key.public_key().as_slice(),
                    sealed.nonce().as_slice(),
                    sealed.ciphertext(),
                    quota_db,
                ],
            )
            .map_err(storage)?;
            Ok(SqldRegistryRecord {
                state: SqldRegistryState::Intent,
                sql_namespace,
                public_key: *key.public_key(),
                quota_bytes,
                generation: REGISTRY_GENERATION,
            })
        })
    }

    /// Indica se há qualquer registro para o nome exato, em qualquer
    /// namespace RBAC e estado. Autentica a master e verifica o orçamento
    /// pela mesma transação que as demais operações, sem abrir envelopes de
    /// donos ou retornar dados de registros.
    pub fn has_worker_name(&self, full_worker_name: &str) -> Result<bool, SqldRegistryError> {
        if !valid_text(full_worker_name) {
            return Err(SqldRegistryError::InvalidWorker);
        }
        self.with_tx(|tx| {
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqld_registry WHERE worker = ?1)",
                params![full_worker_name],
                |row| row.get(0),
            )
            .map_err(storage)
        })
    }

    /// Metadata do dono, depois de abrir o envelope e conferir a chave.
    pub fn get(
        &self,
        owner: &SqldRegistryOwner<'_>,
    ) -> Result<Option<SqldRegistryRecord>, SqldRegistryError> {
        validate_owner(owner)?;
        self.with_tx(|tx| Ok(self.find(tx, owner)?.map(|found| found.record)))
    }

    /// Registra que a chave pública `observed_remote_pubkey` (já validada
    /// pelo chamador contra um GET remoto fresco; este método NÃO consulta a
    /// rede) bate com a gravada. `Intent`/`Proven`/`Ready` com chave igual
    /// avançam para `Proven` (um `Ready` continua `Ready`). Chave diferente
    /// grava `Conflict` de forma persistente e devolve
    /// [`SqldRegistryError::PublicKeyMismatch`]; a chave nunca é trocada.
    pub fn mark_proven(
        &self,
        owner: &SqldRegistryOwner<'_>,
        observed_remote_pubkey: [u8; PUBLIC_KEY_LEN],
    ) -> Result<SqldRegistryRecord, SqldRegistryError> {
        validate_owner(owner)?;
        let outcome = self.with_tx(|tx| {
            let mut found = self.find(tx, owner)?.ok_or(SqldRegistryError::NotFound)?;
            if found.record.state == SqldRegistryState::Conflict {
                return Err(SqldRegistryError::Conflict);
            }
            if !ct_eq(&found.record.public_key, &observed_remote_pubkey) {
                set_state(tx, found.id, SqldRegistryState::Conflict)?;
                return Ok(None);
            }
            if found.record.state == SqldRegistryState::Intent {
                set_state(tx, found.id, SqldRegistryState::Proven)?;
                found.record.state = SqldRegistryState::Proven;
            }
            Ok(Some(found.record))
        })?;
        outcome.ok_or(SqldRegistryError::PublicKeyMismatch)
    }

    /// Host-only: grava `Conflict` terminal quando o host provou, por um GET
    /// remoto fresco, que a chave pública remota do namespace está ausente
    /// ou é inválida (nada a comparar com `mark_proven`). Mesma transação,
    /// master e integridade de `mark_proven`; nunca troca a chave nem a
    /// quota. Idempotente: um dono já em `Conflict` devolve o mesmo registro.
    /// Não há caminho de volta (`mark_*` e token seguem negados).
    pub fn mark_conflict(
        &self,
        owner: &SqldRegistryOwner<'_>,
    ) -> Result<SqldRegistryRecord, SqldRegistryError> {
        validate_owner(owner)?;
        self.with_tx(|tx| {
            let mut found = self.find(tx, owner)?.ok_or(SqldRegistryError::NotFound)?;
            if found.record.state != SqldRegistryState::Conflict {
                set_state(tx, found.id, SqldRegistryState::Conflict)?;
                found.record.state = SqldRegistryState::Conflict;
            }
            Ok(found.record)
        })
    }

    /// `Proven`/`Ready` → `Ready`. `Intent` é transição inválida; `Conflict`
    /// nega.
    pub fn mark_ready(
        &self,
        owner: &SqldRegistryOwner<'_>,
    ) -> Result<SqldRegistryRecord, SqldRegistryError> {
        validate_owner(owner)?;
        self.with_tx(|tx| {
            let mut found = self.find(tx, owner)?.ok_or(SqldRegistryError::NotFound)?;
            match found.record.state {
                SqldRegistryState::Conflict => Err(SqldRegistryError::Conflict),
                SqldRegistryState::Intent => Err(SqldRegistryError::InvalidTransition),
                SqldRegistryState::Ready => Ok(found.record),
                SqldRegistryState::Proven => {
                    set_state(tx, found.id, SqldRegistryState::Ready)?;
                    found.record.state = SqldRegistryState::Ready;
                    Ok(found.record)
                }
            }
        })
    }

    /// Assina o JWT escopado ao namespace SQL VINCULADO ao dono (não é
    /// parâmetro). Só `Proven`/`Ready`; `Intent` e `Conflict` negam. A chave
    /// privada é aberta internamente e nunca sai deste módulo.
    pub fn sign_scoped_token(
        &self,
        owner: &SqldRegistryOwner<'_>,
        now_unix: i64,
        ttl_seconds: u64,
    ) -> Result<SqldScopedToken, SqldRegistryError> {
        validate_owner(owner)?;
        let found = self.with_tx(|tx| self.find(tx, owner)?.ok_or(SqldRegistryError::NotFound))?;
        match found.record.state {
            SqldRegistryState::Intent => Err(SqldRegistryError::NotProven),
            SqldRegistryState::Conflict => Err(SqldRegistryError::Conflict),
            SqldRegistryState::Proven | SqldRegistryState::Ready => Ok(found
                .key
                .sign_scoped_token(&found.record.sql_namespace, now_unix, ttl_seconds)?),
        }
    }
}

impl Drop for SqldProvisionRegistry {
    /// Fecha a `Connection` DENTRO do gate global (ordem gate -> conexão),
    /// sem vazar o fd nem a conexão. O `close` do SQLite pode soltar locks
    /// POSIX do arquivo; com o gate nenhum outro handle está numa transação.
    fn drop(&mut self) {
        let _gate = fd_gate();
        let slot = self
            .conn
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(slot.take());
    }
}

/// Gate global do processo para TODOS os handles do registro.
///
/// Por que: no Linux o SQLite usa locks POSIX (`fcntl`), e um `close()` de
/// QUALQUER fd do mesmo arquivo no processo cancela os locks que outra
/// conexão SQLite do mesmo processo segura (SQLite "How To Corrupt", seção
/// "POSIX advisory locks canceled by a close()"; lockingv3). O preflight
/// bruto de `open` abre e fecha um fd do arquivo; sem coordenação, isso
/// derruba o lock de escrita de outro handle e deixa um processo FILHO
/// escrever. Todo trabalho com fd/lock (preflight bruto, abertura do SQLite,
/// toda transação, close/Drop da `Connection`, caminhos de erro) roda sob
/// este mutex. Ordem fixa: gate global -> mutex da conexão. Nada aqui dá
/// `await` nem faz RPC. Limite: só coordena handles deste módulo no mesmo
/// processo; outro código que abra o arquivo por fora do módulo não é
/// coberto (e outro processo é coberto pelos locks do SQLite).
static FD_GATE: Mutex<()> = Mutex::new(());

fn fd_gate() -> MutexGuard<'static, ()> {
    // O dado protegido é `()`: um panic com o gate preso não deixa estado
    // inconsistente, então o envenenamento é ignorado.
    FD_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn master_check_owner() -> SqldSignerOwner<'static> {
    SqldSignerOwner {
        worker: MASTER_CHECK_WORKER,
        rbac_namespace: None,
        generation: REGISTRY_GENERATION,
    }
}

/// Abre o envelope de master check dos metadados e compara a chave pública.
/// Master errada ou metadados adulterados: `Integrity` (fail-closed, sem
/// reparo, sem reescrever o check).
fn verify_master_check(
    tx: &Transaction<'_>,
    master: &[u8; MASTER_KEY_LEN],
) -> Result<(), SqldRegistryError> {
    let integrity = SqldRegistryError::Integrity;
    let (public, nonce, ciphertext): (Vec<u8>, Vec<u8>, Vec<u8>) = tx
        .query_row(
            "SELECT check_public_key, check_nonce, check_ciphertext \
             FROM sqld_registry_meta WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..) => integrity,
            other => storage(other),
        })?;
    let stored: [u8; PUBLIC_KEY_LEN] = public.as_slice().try_into().map_err(|_| integrity)?;
    let sealed = SealedSqldSigningKey::from_parts(&nonce, &ciphertext).map_err(|_| integrity)?;
    let key =
        SqldSigningKey::open(&sealed, master, &master_check_owner()).map_err(|_| integrity)?;
    if !ct_eq(key.public_key(), &stored) {
        return Err(integrity);
    }
    Ok(())
}

fn set_state(
    tx: &Transaction<'_>,
    id: i64,
    state: SqldRegistryState,
) -> Result<(), SqldRegistryError> {
    let changed = tx
        .execute(
            "UPDATE sqld_registry SET state = ?1 WHERE id = ?2",
            params![state.to_db(), id],
        )
        .map_err(storage)?;
    if changed != 1 {
        return Err(SqldRegistryError::Storage);
    }
    Ok(())
}

/// Soma as quotas de TODAS as linhas com aritmética checada.
fn sum_quotas(tx: &Transaction<'_>) -> Result<u64, SqldRegistryError> {
    let mut stmt = tx
        .prepare("SELECT quota_bytes FROM sqld_registry")
        .map_err(storage)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, i64>(0))
        .map_err(storage)?;
    let mut total: u64 = 0;
    for row in rows {
        let quota =
            u64::try_from(row.map_err(storage)?).map_err(|_| SqldRegistryError::Integrity)?;
        total = total
            .checked_add(quota)
            .ok_or(SqldRegistryError::BudgetExceeded)?;
    }
    Ok(total)
}

/// Relê o orçamento persistido (dentro da transação de escrita) e recusa se
/// mudou desde o `open`.
fn check_budget(tx: &Transaction<'_>, budget: u64) -> Result<(), SqldRegistryError> {
    let stored: i64 = tx
        .query_row(
            "SELECT managed_budget_bytes FROM sqld_registry_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if u64::try_from(stored).ok() != Some(budget) {
        return Err(SqldRegistryError::BudgetMismatch);
    }
    Ok(())
}

fn schema_sql(tx: &Transaction<'_>, name: &str) -> Result<Option<String>, SqldRegistryError> {
    tx.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(storage)
}

/// Dentro de uma transação `IMMEDIATE`: cria o schema só num arquivo vazio
/// (recém-criado por nós, ou de 0 bytes) e senão exige schema, versão e
/// orçamento conhecidos. Nunca "repara" um banco alheio ou corrompido.
fn init_or_verify(
    tx: &Transaction<'_>,
    may_init: bool,
    budget: u64,
    master: &[u8; MASTER_KEY_LEN],
) -> Result<(), SqldRegistryError> {
    let app_id: i64 = tx
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(storage)?;
    let objects: i64 = tx
        .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
        .map_err(storage)?;
    if app_id == 0 && objects == 0 {
        if !may_init {
            return Err(SqldRegistryError::ForeignDatabase);
        }
        tx.execute_batch(META_SQL).map_err(storage)?;
        tx.execute_batch(REGISTRY_SQL).map_err(storage)?;
        tx.execute_batch(&format!("PRAGMA application_id = {APPLICATION_ID}"))
            .map_err(storage)?;
        let budget_db = i64::try_from(budget).map_err(|_| SqldRegistryError::InvalidBudget)?;
        let check = SqldSigningKey::generate()?;
        let sealed = check.seal(master, &master_check_owner())?;
        tx.execute(
            "INSERT INTO sqld_registry_meta (id, schema_version, managed_budget_bytes, \
             check_public_key, check_nonce, check_ciphertext) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
            params![
                REGISTRY_SCHEMA_VERSION,
                budget_db,
                check.public_key().as_slice(),
                sealed.nonce().as_slice(),
                sealed.ciphertext(),
            ],
        )
        .map_err(storage)?;
        return Ok(());
    }
    if app_id != APPLICATION_ID {
        return Err(SqldRegistryError::ForeignDatabase);
    }
    // Uma versão de schema diferente (inclusive o ledger v1 sem master check,
    // que nunca foi integrado) é recusada com clareza, sem migração e sem
    // tocar o arquivo.
    let meta_sql =
        schema_sql(tx, "sqld_registry_meta")?.ok_or(SqldRegistryError::ForeignDatabase)?;
    let version: Option<i64> = tx
        .query_row(
            "SELECT schema_version FROM sqld_registry_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| SqldRegistryError::ForeignDatabase)?;
    let version = version.ok_or(SqldRegistryError::ForeignDatabase)?;
    if version != REGISTRY_SCHEMA_VERSION {
        return Err(SqldRegistryError::UnsupportedSchema);
    }
    if meta_sql != META_SQL {
        return Err(SqldRegistryError::ForeignDatabase);
    }
    let stored_budget: i64 = tx
        .query_row(
            "SELECT managed_budget_bytes FROM sqld_registry_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if schema_sql(tx, "sqld_registry")?.as_deref() != Some(REGISTRY_SQL) {
        return Err(SqldRegistryError::ForeignDatabase);
    }
    // A master é autenticada antes de qualquer outro uso do ledger.
    verify_master_check(tx, master)?;
    if u64::try_from(stored_budget).ok() != Some(budget) {
        return Err(SqldRegistryError::BudgetMismatch);
    }
    Ok(())
}

#[cfg(unix)]
mod secure_file {
    //! Abertura segura do arquivo (ver "Arquivo" na documentação do módulo).

    use std::ffi::OsString;
    use std::os::fd::AsFd;
    use std::path::Path;

    use rusqlite::{Connection, OpenFlags};
    use rustix::fs::{self, AtFlags, Mode, OFlags, Stat, CWD};
    use rustix::io::Errno;

    use super::SqldRegistryError;

    const S_IFMT: u32 = 0o170_000;
    const S_IFREG: u32 = 0o100_000;
    const S_IFDIR: u32 = 0o040_000;

    #[allow(clippy::unnecessary_cast)]
    fn mode_bits(st: &Stat) -> u32 {
        st.st_mode as u32
    }

    #[allow(clippy::unnecessary_cast)]
    fn nlink(st: &Stat) -> u64 {
        st.st_nlink as u64
    }

    #[allow(clippy::unnecessary_cast)]
    fn identity(st: &Stat) -> (u64, u64) {
        (st.st_dev as u64, st.st_ino as u64)
    }

    fn map_errno(errno: Errno) -> SqldRegistryError {
        match errno {
            Errno::LOOP | Errno::NOTDIR | Errno::NOENT | Errno::ISDIR | Errno::MLINK => {
                SqldRegistryError::UnsafePath
            }
            Errno::ACCESS | Errno::PERM => SqldRegistryError::WeakPermissions,
            _ => SqldRegistryError::Storage,
        }
    }

    fn check_private(
        st: &Stat,
        euid: u32,
        file_type: u32,
        mode: u32,
    ) -> Result<(), SqldRegistryError> {
        if mode_bits(st) & S_IFMT != file_type {
            return Err(SqldRegistryError::UnsafePath);
        }
        if st.st_uid != euid || mode_bits(st) & 0o7777 != mode {
            return Err(SqldRegistryError::WeakPermissions);
        }
        Ok(())
    }

    /// Sidecar (`-journal`): se existir, mesmas checagens do DB. `-wal`/`-shm`
    /// presentes são recusados (o ledger nunca usa WAL).
    fn check_sidecars(
        dir: &impl AsFd,
        name: &std::ffi::OsStr,
        euid: u32,
    ) -> Result<(), SqldRegistryError> {
        let sidecar = |suffix: &str| {
            let mut full: OsString = name.to_os_string();
            full.push(suffix);
            full
        };
        match fs::statat(dir, sidecar("-journal"), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => {
                check_private(&st, euid, S_IFREG, 0o600)?;
                if nlink(&st) != 1 {
                    return Err(SqldRegistryError::UnsafePath);
                }
            }
            Err(Errno::NOENT) => {}
            Err(_) => return Err(SqldRegistryError::UnsafePath),
        }
        for suffix in ["-wal", "-shm"] {
            match fs::statat(dir, sidecar(suffix), AtFlags::SYMLINK_NOFOLLOW) {
                Err(Errno::NOENT) => {}
                _ => return Err(SqldRegistryError::UnsafePath),
            }
        }
        Ok(())
    }

    /// Devolve a conexão e se o arquivo está vazio/recém-criado (pode
    /// receber o schema).
    pub(super) fn open_connection(path: &Path) -> Result<(Connection, bool), SqldRegistryError> {
        let name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or(SqldRegistryError::UnsafePath)?;
        if path.file_name().is_none() || path.ends_with("..") {
            return Err(SqldRegistryError::UnsafePath);
        }
        // O SQLite abre por caminho e trata o nome `:memory:` (e `""`) como
        // banco especial, e (bundled, `SQLITE_USE_URI`) um caminho que
        // começa com `file:` como URI (`mode=memory`, `vfs=`, `nolock=`...),
        // não como arquivo: um ledger "persistente" ficaria só em memória (e
        // o `openat` abaixo criaria um arquivo vazio órfão). Recusa antes de
        // criar qualquer coisa, para caminho relativo ou absoluto.
        {
            use std::os::unix::ffi::OsStrExt;
            // O SQLite recebe o caminho INTEIRO: `file:private/ledger.db` tem
            // basename `ledger.db`, mas seria aberto como URI
            // (`private/ledger.db`), não o arquivo que o preflight checou.
            // Compara os bytes crus do caminho completo (sem canonicalize, que
            // seguiria symlink, e sem conversão com perda).
            let bytes = name.as_bytes();
            if bytes == b":memory:"
                || bytes.starts_with(b"file:")
                || path.as_os_str().as_bytes().starts_with(b"file:")
            {
                return Err(SqldRegistryError::UnsafePath);
            }
        }
        let parent = match path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let euid = rustix::process::geteuid().as_raw();

        let dir = fs::openat(
            CWD,
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(map_errno)?;
        let dir_stat = fs::fstat(&dir).map_err(map_errno)?;
        check_private(&dir_stat, euid, S_IFDIR, 0o700)?;

        let (file, created) = match fs::openat(
            &dir,
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        ) {
            Ok(file) => {
                // O umask pode ter reduzido o modo; o arquivo é nosso.
                fs::fchmod(&file, Mode::from_bits_truncate(0o600)).map_err(map_errno)?;
                (file, true)
            }
            Err(Errno::EXIST) => {
                let file = fs::openat(
                    &dir,
                    name,
                    OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(map_errno)?;
                (file, false)
            }
            Err(err) => return Err(map_errno(err)),
        };
        let st = fs::fstat(&file).map_err(map_errno)?;
        check_private(&st, euid, S_IFREG, 0o600)?;
        if nlink(&st) != 1 {
            return Err(SqldRegistryError::UnsafePath);
        }
        check_sidecars(&dir, name, euid)?;
        let may_init = created || st.st_size == 0;

        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(super::storage)?;
        // O SQLite abriu por caminho: confirma que o caminho ainda aponta
        // para o arquivo checado (mesmo dev/inode, sem symlink).
        let again = fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).map_err(map_errno)?;
        if identity(&again) != identity(&st) {
            return Err(SqldRegistryError::UnsafePath);
        }
        Ok((conn, may_init))
    }
}

#[cfg(not(unix))]
mod secure_file {
    use std::path::Path;

    use rusqlite::Connection;

    use super::SqldRegistryError;

    pub(super) fn open_connection(_path: &Path) -> Result<(Connection, bool), SqldRegistryError> {
        Err(SqldRegistryError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqld_signer::SqldSignerError;
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
    use serde_json::Value;
    use std::sync::{Arc, Barrier};

    const MIB: u64 = 1024 * 1024;
    const MASTER: [u8; MASTER_KEY_LEN] = [7u8; MASTER_KEY_LEN];
    const OTHER_MASTER: [u8; MASTER_KEY_LEN] = [9u8; MASTER_KEY_LEN];
    const BUDGET: u64 = 64 * MIB;
    const Q: u64 = 16 * MIB;

    fn master() -> Zeroizing<[u8; MASTER_KEY_LEN]> {
        Zeroizing::new(MASTER)
    }

    fn owner<'a>(worker: &'a str, ns: Option<&'a str>) -> SqldRegistryOwner<'a> {
        SqldRegistryOwner {
            worker,
            rbac_namespace: ns,
        }
    }

    fn mem(budget: u64) -> SqldProvisionRegistry {
        SqldProvisionRegistry::in_memory(master(), budget).unwrap()
    }

    #[cfg(unix)]
    fn private_dir() -> tempfile::TempDir {
        // `tempfile` segue o umask; força o 0700 que o ledger exige (o
        // diretório é do próprio teste).
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    #[cfg(unix)]
    fn open_at(dir: &Path, budget: u64) -> Result<SqldProvisionRegistry, SqldRegistryError> {
        SqldProvisionRegistry::open(&dir.join("registry.db"), master(), budget)
    }

    #[cfg(unix)]
    fn raw(dir: &Path) -> Connection {
        Connection::open(dir.join("registry.db")).unwrap()
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn verify_jwt(pubkey: &[u8; 32], jwt: &str) -> Value {
        use base64::Engine;
        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pubkey);
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.leeway = 0;
        validation.required_spec_claims = ["exp".to_string()].into_iter().collect();
        decode::<Value>(
            jwt,
            &DecodingKey::from_ed_components(&x).unwrap(),
            &validation,
        )
        .unwrap()
        .claims
    }

    #[test]
    fn has_worker_name_matches_exact_name_in_every_namespace_and_state() {
        for (ns, state) in [
            (None, "intent"),
            (Some("@t"), "proven"),
            (Some("@other"), "ready"),
            (Some("@other"), "conflict"),
        ] {
            let reg = mem(BUDGET);
            let who = owner("app", ns);
            assert_eq!(reg.has_worker_name("app"), Ok(false));
            let rec = reg.ensure_intent(&who, Q).unwrap();
            match state {
                "intent" => {}
                "proven" => {
                    reg.mark_proven(&who, rec.public_key).unwrap();
                }
                "ready" => {
                    reg.mark_proven(&who, rec.public_key).unwrap();
                    reg.mark_ready(&who).unwrap();
                }
                "conflict" => {
                    reg.mark_conflict(&who).unwrap();
                }
                _ => unreachable!(),
            }
            let expected = match state {
                "intent" => SqldRegistryState::Intent,
                "proven" => SqldRegistryState::Proven,
                "ready" => SqldRegistryState::Ready,
                "conflict" => SqldRegistryState::Conflict,
                _ => unreachable!(),
            };
            assert_eq!(reg.get(&who).unwrap().unwrap().state, expected);
            assert_eq!(reg.has_worker_name("app"), Ok(true), "{ns:?}/{state}");
            for other in ["ap", "app-longer", "App", "app%", "app_", "app' OR 1=1 --"] {
                assert_eq!(reg.has_worker_name(other), Ok(false), "{ns:?}/{state}");
            }
        }
    }

    #[test]
    fn has_worker_name_rejects_invalid_input_and_fails_closed_on_ledger_error() {
        let reg = mem(BUDGET);
        for invalid in ["", "bad\nname", "bad\0name"] {
            assert_eq!(
                reg.has_worker_name(invalid),
                Err(SqldRegistryError::InvalidWorker)
            );
        }
        assert_eq!(
            reg.has_worker_name(&"x".repeat(MAX_NAME_LEN + 1)),
            Err(SqldRegistryError::InvalidWorker)
        );
        assert_eq!(reg.has_worker_name("app"), Ok(false));
        reg.with_conn(|conn| {
            conn.execute("UPDATE sqld_registry_meta SET check_nonce = x'00'", [])
                .unwrap();
        });
        assert!(
            reg.has_worker_name("app").is_err(),
            "ledger failure cannot mean absent"
        );
    }

    // ---------- identidade, idempotência, quota ----------

    #[test]
    fn ensure_intent_is_idempotent_for_same_quota_and_conflicts_on_other() {
        let reg = mem(BUDGET);
        let o = owner("app", None);
        let a = reg.ensure_intent(&o, Q).unwrap();
        let b = reg.ensure_intent(&o, Q).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.state, SqldRegistryState::Intent);
        assert!(a.sql_namespace.starts_with("edger_"));
        assert_eq!(a.sql_namespace.len(), 6 + 64);
        assert_eq!(
            reg.ensure_intent(&o, Q + QUOTA_ALIGN_BYTES),
            Err(SqldRegistryError::QuotaConflict)
        );
        // o conflito não alterou a reserva
        assert_eq!(reg.ensure_intent(&o, Q).unwrap(), a);
        assert_eq!(reg.get(&o).unwrap(), Some(a));
        assert_eq!(reg.get(&owner("other", None)).unwrap(), None);
    }

    #[test]
    fn owners_none_some_and_concatenation_ambiguity_are_distinct() {
        let reg = mem(512 * MIB);
        let owners = [
            owner("ab", Some("c")),
            owner("a", Some("bc")),
            owner("ab", None),
            owner("abc", None),
            owner("a", Some("b")),
            owner("ab", Some("cd")),
            owner("abc", Some("d")),
        ];
        let mut seen_ns = std::collections::HashSet::new();
        let mut seen_pk = std::collections::HashSet::new();
        for o in &owners {
            let rec = reg.ensure_intent(o, Q).unwrap();
            assert!(seen_ns.insert(rec.sql_namespace.clone()), "ns collision");
            assert!(seen_pk.insert(rec.public_key), "key reuse");
        }
        assert_eq!(seen_ns.len(), owners.len());
        // determinístico: mesmo dono em outro ledger gera o mesmo namespace
        let other = mem(BUDGET);
        assert_eq!(
            other.ensure_intent(&owners[0], Q).unwrap().sql_namespace,
            reg.get(&owners[0]).unwrap().unwrap().sql_namespace
        );
        // sem trim/normalização
        let spaced = reg.ensure_intent(&owner(" ab", Some("c")), Q).unwrap();
        assert!(seen_ns.insert(spaced.sql_namespace));
    }

    #[test]
    fn namespace_encoding_is_length_prefixed_even_for_inputs_validation_rejects() {
        // `validate_owner` já impede bytes de controle, então estes dois donos
        // nunca chegam à derivação pela API; aqui a derivação é chamada direto
        // para provar que o prefixo de tamanho do worker separa os campos
        // (sem ele os dois fluxos de bytes seriam idênticos).
        let a = owner("a", Some("x\u{1}\0\0\0\0"));
        let b = owner("a\u{1}\0\0\0\u{6}x", Some(""));
        assert_ne!(derive_sql_namespace(&a), derive_sql_namespace(&b));
    }

    #[test]
    fn invalid_owners_are_rejected_with_static_errors() {
        let reg = mem(BUDGET);
        let long = "x".repeat(MAX_NAME_LEN + 1);
        assert_eq!(
            reg.ensure_intent(&owner("", None), Q),
            Err(SqldRegistryError::InvalidWorker)
        );
        assert_eq!(
            reg.ensure_intent(&owner("a\nb", None), Q),
            Err(SqldRegistryError::InvalidWorker)
        );
        assert_eq!(
            reg.ensure_intent(&owner("a\0b", None), Q),
            Err(SqldRegistryError::InvalidWorker)
        );
        assert_eq!(
            reg.ensure_intent(&owner(&long, None), Q),
            Err(SqldRegistryError::InvalidWorker)
        );
        assert_eq!(
            reg.ensure_intent(&owner("a", Some("")), Q),
            Err(SqldRegistryError::InvalidRbacNamespace)
        );
        assert_eq!(
            reg.ensure_intent(&owner("a", Some("t\u{7}")), Q),
            Err(SqldRegistryError::InvalidRbacNamespace)
        );
        assert_eq!(
            reg.mark_ready(&owner("", None)),
            Err(SqldRegistryError::InvalidWorker)
        );
        let max = "y".repeat(MAX_NAME_LEN);
        assert!(reg.ensure_intent(&owner(&max, Some(&max)), Q).is_ok());
    }

    // ---------- estados e token ----------

    #[test]
    fn intent_denies_token_and_proven_ready_sign_only_bound_namespace() {
        let reg = mem(BUDGET);
        let a = owner("app-a", Some("team"));
        let b = owner("app-b", None);
        let rec_a = reg.ensure_intent(&a, Q).unwrap();
        let rec_b = reg.ensure_intent(&b, Q).unwrap();
        assert_eq!(
            reg.sign_scoped_token(&a, now(), 60).unwrap_err(),
            SqldRegistryError::NotProven
        );
        reg.mark_proven(&a, rec_a.public_key).unwrap();
        reg.mark_proven(&b, rec_b.public_key).unwrap();
        let ta = reg.sign_scoped_token(&a, now(), 60).unwrap();
        let claims = verify_jwt(&rec_a.public_key, ta.expose_jwt());
        assert_eq!(claims["p"]["rw"]["ns"][0], rec_a.sql_namespace.as_str());
        assert_eq!(claims["p"]["ddl"]["ns"][0], rec_a.sql_namespace.as_str());
        assert_eq!(claims["p"]["rw"]["ns"].as_array().unwrap().len(), 1);
        reg.mark_ready(&a).unwrap();
        let tb = reg.sign_scoped_token(&b, now(), 60).unwrap();
        let claims_b = verify_jwt(&rec_b.public_key, tb.expose_jwt());
        assert_eq!(claims_b["p"]["rw"]["ns"][0], rec_b.sql_namespace.as_str());
        assert_ne!(rec_a.sql_namespace, rec_b.sql_namespace);
        // chaves distintas: o token de A não verifica com a chave de B
        let ready = reg.sign_scoped_token(&a, now(), 60).unwrap();
        let x = {
            use base64::Engine;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rec_b.public_key)
        };
        let mut v = Validation::new(Algorithm::EdDSA);
        v.validate_exp = false;
        v.required_spec_claims.clear();
        assert!(decode::<Value>(
            ready.expose_jwt(),
            &DecodingKey::from_ed_components(&x).unwrap(),
            &v
        )
        .is_err());
        // erros do signer propagam (ttl 0, tempo negativo)
        assert_eq!(
            reg.sign_scoped_token(&a, now(), 0).unwrap_err(),
            SqldRegistryError::Signer(SqldSignerError::InvalidTtl)
        );
        assert_eq!(
            reg.sign_scoped_token(&a, -1, 60).unwrap_err(),
            SqldRegistryError::Signer(SqldSignerError::InvalidTime)
        );
        assert_eq!(
            reg.sign_scoped_token(&owner("nobody", None), now(), 60)
                .unwrap_err(),
            SqldRegistryError::NotFound
        );
    }

    #[test]
    fn state_transitions_valid_and_invalid() {
        let reg = mem(BUDGET);
        let o = owner("app", None);
        let pk = reg.ensure_intent(&o, Q).unwrap().public_key;
        assert_eq!(
            reg.mark_ready(&o).unwrap_err(),
            SqldRegistryError::InvalidTransition
        );
        assert_eq!(
            reg.get(&o).unwrap().unwrap().state,
            SqldRegistryState::Intent
        );
        assert_eq!(
            reg.mark_proven(&owner("none", None), pk).unwrap_err(),
            SqldRegistryError::NotFound
        );
        assert_eq!(
            reg.mark_ready(&owner("none", None)).unwrap_err(),
            SqldRegistryError::NotFound
        );
        assert_eq!(
            reg.mark_proven(&o, pk).unwrap().state,
            SqldRegistryState::Proven
        );
        assert_eq!(
            reg.mark_proven(&o, pk).unwrap().state,
            SqldRegistryState::Proven
        );
        assert_eq!(reg.mark_ready(&o).unwrap().state, SqldRegistryState::Ready);
        assert_eq!(reg.mark_ready(&o).unwrap().state, SqldRegistryState::Ready);
        // `Ready` não regride por mark_proven nem por ensure_intent
        assert_eq!(
            reg.mark_proven(&o, pk).unwrap().state,
            SqldRegistryState::Ready
        );
        assert_eq!(
            reg.ensure_intent(&o, Q).unwrap().state,
            SqldRegistryState::Ready
        );
    }

    fn assert_conflict_everywhere(
        reg: &SqldProvisionRegistry,
        o: &SqldRegistryOwner<'_>,
        pk: [u8; 32],
    ) {
        assert_eq!(
            reg.get(o).unwrap().unwrap().state,
            SqldRegistryState::Conflict
        );
        assert_eq!(reg.mark_ready(o).unwrap_err(), SqldRegistryError::Conflict);
        // nem a chave CORRETA reabilita um Conflict
        assert_eq!(
            reg.mark_proven(o, pk).unwrap_err(),
            SqldRegistryError::Conflict
        );
        assert_eq!(
            reg.sign_scoped_token(o, now(), 60).unwrap_err(),
            SqldRegistryError::Conflict
        );
    }

    // ---------- mark_conflict (host-only, terminal, idempotente) ----------

    #[test]
    fn mark_conflict_is_terminal_idempotent_and_keeps_key_and_reservation() {
        for from in [0u8, 1, 2] {
            let reg = mem(BUDGET);
            let o = owner("app", Some("t"));
            let rec = reg.ensure_intent(&o, Q).unwrap();
            if from >= 1 {
                reg.mark_proven(&o, rec.public_key).unwrap();
            }
            if from == 2 {
                reg.mark_ready(&o).unwrap();
            }
            let first = reg.mark_conflict(&o).unwrap();
            assert_eq!(first.state, SqldRegistryState::Conflict, "from {from}");
            assert_eq!(first.public_key, rec.public_key);
            assert_eq!(first.quota_bytes, Q);
            // idempotente
            assert_eq!(reg.mark_conflict(&o).unwrap(), first);
            assert_conflict_everywhere(&reg, &o, rec.public_key);
            // a reserva continua contando
            let other = owner("other", None);
            assert_eq!(
                reg.ensure_intent(&other, BUDGET).unwrap_err(),
                SqldRegistryError::BudgetExceeded
            );
        }
    }

    #[test]
    fn mark_conflict_refuses_unknown_and_invalid_owner_and_isolates_owners() {
        let reg = mem(BUDGET);
        assert_eq!(
            reg.mark_conflict(&owner("ghost", None)).unwrap_err(),
            SqldRegistryError::NotFound
        );
        assert_eq!(
            reg.mark_conflict(&owner("", None)).unwrap_err(),
            SqldRegistryError::InvalidWorker
        );
        // um dono não afeta o outro
        let a = owner("a", None);
        let b = owner("b", None);
        reg.ensure_intent(&a, Q).unwrap();
        reg.ensure_intent(&b, Q).unwrap();
        reg.mark_conflict(&a).unwrap();
        assert_eq!(
            reg.get(&b).unwrap().unwrap().state,
            SqldRegistryState::Intent
        );
    }

    #[cfg(unix)]
    #[test]
    fn mark_conflict_persists_across_reopen_and_checks_the_master_first() {
        let dir = private_dir();
        let o = owner("app", None);
        let rec = {
            let reg = open_at(dir.path(), BUDGET).unwrap();
            let rec = reg.ensure_intent(&o, Q).unwrap();
            reg.mark_conflict(&o).unwrap();
            rec
        };
        let reg = open_at(dir.path(), BUDGET).unwrap();
        assert_conflict_everywhere(&reg, &o, rec.public_key);
        // master errada: `open` já falha (Integrity), sem marcar nada
        let wrong = SqldProvisionRegistry::open(
            &dir.path().join("registry.db"),
            Zeroizing::new(OTHER_MASTER),
            BUDGET,
        );
        assert_eq!(wrong.unwrap_err(), SqldRegistryError::Integrity);
    }

    #[cfg(unix)]
    #[test]
    fn mark_conflict_fails_closed_on_a_tampered_record() {
        let dir = private_dir();
        let o = owner("app", None);
        let reg = open_at(dir.path(), BUDGET).unwrap();
        reg.ensure_intent(&o, Q).unwrap();
        raw(dir.path())
            .execute("UPDATE sqld_registry SET public_key = zeroblob(32)", [])
            .unwrap();
        assert_eq!(
            reg.mark_conflict(&o).unwrap_err(),
            SqldRegistryError::Integrity
        );
    }

    #[test]
    fn mismatched_pubkey_is_a_persistent_conflict_without_rekey() {
        let reg = mem(BUDGET);
        let o = owner("app", None);
        let rec = reg.ensure_intent(&o, Q).unwrap();
        let mut wrong = rec.public_key;
        wrong[31] ^= 1;
        assert_eq!(
            reg.mark_proven(&o, wrong).unwrap_err(),
            SqldRegistryError::PublicKeyMismatch
        );
        assert_conflict_everywhere(&reg, &o, rec.public_key);
        // a chave gravada não mudou (nem foi substituída pela observada)
        assert_eq!(reg.get(&o).unwrap().unwrap().public_key, rec.public_key);
        assert_eq!(
            reg.ensure_intent(&o, Q).unwrap().state,
            SqldRegistryState::Conflict
        );
    }

    #[test]
    fn mismatch_after_proven_and_ready_also_conflicts() {
        for ready in [false, true] {
            let reg = mem(BUDGET);
            let o = owner("app", Some("t"));
            let rec = reg.ensure_intent(&o, Q).unwrap();
            reg.mark_proven(&o, rec.public_key).unwrap();
            if ready {
                reg.mark_ready(&o).unwrap();
            }
            assert_eq!(
                reg.mark_proven(&o, [0u8; 32]).unwrap_err(),
                SqldRegistryError::PublicKeyMismatch
            );
            assert_conflict_everywhere(&reg, &o, rec.public_key);
        }
    }

    // ---------- orçamento ----------

    #[test]
    fn budget_and_quota_validation() {
        let m = || master();
        for bad in [
            0,
            MIB,
            16 * MIB - QUOTA_ALIGN_BYTES,
            16 * MIB + 1,
            u64::MAX,
            (1 << 63) + 4096,
        ] {
            assert_eq!(
                SqldProvisionRegistry::in_memory(m(), bad).unwrap_err(),
                SqldRegistryError::InvalidBudget,
                "budget {bad}"
            );
        }
        // i64::MAX não é múltiplo de 4096; o maior alinhado ≤ i64::MAX é aceito
        let max_aligned = (i64::MAX as u64) & !(QUOTA_ALIGN_BYTES - 1);
        assert!(SqldProvisionRegistry::in_memory(m(), max_aligned).is_ok());
        let reg = mem(32 * MIB);
        let o = owner("a", None);
        for bad in [
            0,
            Q - QUOTA_ALIGN_BYTES,
            Q + 1,
            32 * MIB + QUOTA_ALIGN_BYTES,
            u64::MAX,
        ] {
            assert_eq!(
                reg.ensure_intent(&o, bad).unwrap_err(),
                SqldRegistryError::InvalidQuota,
                "quota {bad}"
            );
        }
        assert_eq!(reg.get(&o).unwrap(), None);
        assert!(reg.ensure_intent(&o, 32 * MIB).is_ok());
    }

    #[test]
    fn reservations_sum_over_all_states_and_never_exceed_budget() {
        let reg = mem(48 * MIB);
        let a = owner("a", None);
        let b = owner("b", None);
        let c = owner("c", None);
        let d = owner("d", None);
        reg.ensure_intent(&a, Q).unwrap(); // Intent
        let rb = reg.ensure_intent(&b, Q).unwrap();
        reg.mark_proven(&b, rb.public_key).unwrap();
        reg.mark_proven(&b, [1u8; 32]).unwrap_err(); // b: Conflict, ainda conta
        reg.ensure_intent(&c, Q).unwrap(); // enche exatamente 48 MiB
        assert_eq!(
            reg.ensure_intent(&d, Q).unwrap_err(),
            SqldRegistryError::BudgetExceeded
        );
        assert_eq!(reg.get(&d).unwrap(), None);
        // repetir um dono existente não consome reserva nova
        assert!(reg.ensure_intent(&a, Q).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn quota_sum_uses_checked_arithmetic() {
        // Linhas adulteradas cuja soma dá a volta em u64 (i64::MAX + i64::MAX +
        // 2 = 2^64): com aritmética que dá a volta, a admissão veria 0.
        let dir = private_dir();
        let reg = open_at(dir.path(), BUDGET).unwrap();
        let conn = raw(dir.path());
        for (i, quota) in [i64::MAX, i64::MAX, 2].into_iter().enumerate() {
            conn.execute(
                "INSERT INTO sqld_registry (worker, rbac_tag, rbac_namespace, sql_namespace, \
                 state, generation, public_key, key_nonce, key_ciphertext, quota_bytes) \
                 VALUES (?1, 0, '', ?2, 0, 1, x'00', x'00', x'00', ?3)",
                params![format!("w{i}"), format!("ns{i}"), quota],
            )
            .unwrap();
        }
        assert_eq!(
            reg.ensure_intent(&owner("new", None), Q).unwrap_err(),
            SqldRegistryError::BudgetExceeded
        );
    }

    // ---------- segredo / redação ----------

    #[test]
    fn debug_display_and_errors_never_echo_secrets_or_names() {
        let reg = mem(BUDGET);
        let name = "very-secret-worker-name";
        let o = owner(name, Some("very-secret-rbac"));
        let rec = reg.ensure_intent(&o, Q).unwrap();
        reg.mark_proven(&o, rec.public_key).unwrap();
        let token = reg.sign_scoped_token(&o, now(), 60).unwrap();
        let mut out = format!("{reg:?} {o:?} {token:?} {rec:?}");
        for err in [
            reg.ensure_intent(&o, Q + QUOTA_ALIGN_BYTES).unwrap_err(),
            reg.mark_proven(&o, [0u8; 32]).unwrap_err(),
            reg.sign_scoped_token(&o, -1, 1).unwrap_err(),
            reg.ensure_intent(&owner("bad\n", None), Q).unwrap_err(),
        ] {
            out.push_str(&format!("{err:?}{err}"));
        }
        assert!(!out.contains(name) && !out.contains("very-secret-rbac"));
        assert!(!out.contains(token.expose_jwt()));
        assert!(out.contains("<redacted>"));
        assert!(!out.contains("07, 07") && !out.contains("[7, 7"));
    }

    #[cfg(unix)]
    #[test]
    fn file_holds_only_sealed_envelope_no_pkcs8_or_jwt() {
        let dir = private_dir();
        let reg = open_at(dir.path(), BUDGET).unwrap();
        let o = owner("app", None);
        let rec = reg.ensure_intent(&o, Q).unwrap();
        reg.mark_proven(&o, rec.public_key).unwrap();
        let _ = reg.sign_scoped_token(&o, now(), 60).unwrap();
        drop(reg);
        let bytes = std::fs::read(dir.path().join("registry.db")).unwrap();
        // OID Ed25519 (1.3.101.112) que todo PKCS#8 do ring carrega
        let oid = [0x06u8, 0x03, 0x2b, 0x65, 0x70];
        assert!(
            !bytes.windows(oid.len()).any(|w| w == oid),
            "pkcs8 oid in file"
        );
        assert!(
            !bytes.windows(3).any(|w| w == b"eyJ"),
            "jwt-like text in file"
        );
        // a chave pública (não secreta) está lá; o envelope tem o tamanho esperado
        assert!(bytes.windows(32).any(|w| w == rec.public_key));
        let ct_len: i64 = raw(dir.path())
            .query_row(
                "SELECT length(key_ciphertext) FROM sqld_registry",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(ct_len > 16 && ct_len <= 256);
    }

    // ---------- persistência e reopen ----------

    #[cfg(unix)]
    #[test]
    fn reopen_keeps_identity_state_pubkey_and_signatures() {
        let dir = private_dir();
        let o = owner("app", Some("team"));
        let (rec, env_before) = {
            let reg = open_at(dir.path(), BUDGET).unwrap();
            let rec = reg.ensure_intent(&o, Q).unwrap();
            reg.mark_proven(&o, rec.public_key).unwrap();
            let env: (Vec<u8>, Vec<u8>) = raw(dir.path())
                .query_row(
                    "SELECT key_nonce, key_ciphertext FROM sqld_registry",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            (rec, env)
        };
        let reg = open_at(dir.path(), BUDGET).unwrap();
        let again = reg.get(&o).unwrap().unwrap();
        assert_eq!(again.public_key, rec.public_key);
        assert_eq!(again.sql_namespace, rec.sql_namespace);
        assert_eq!(again.state, SqldRegistryState::Proven);
        // idempotência entre reopen: mesma identidade e MESMO envelope
        assert_eq!(reg.ensure_intent(&o, Q).unwrap().public_key, rec.public_key);
        let env_after: (Vec<u8>, Vec<u8>) = raw(dir.path())
            .query_row(
                "SELECT key_nonce, key_ciphertext FROM sqld_registry",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(env_before, env_after);
        let token = reg.sign_scoped_token(&o, now(), 60).unwrap();
        let claims = verify_jwt(&rec.public_key, token.expose_jwt());
        assert_eq!(claims["p"]["rw"]["ns"][0], rec.sql_namespace.as_str());
        assert!(claims["exp"].as_u64().unwrap() > now() as u64);
        let rows: i64 = raw(dir.path())
            .query_row("SELECT count(*) FROM sqld_registry", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[cfg(unix)]
    #[test]
    fn conflict_survives_reopen_and_keeps_denying() {
        let dir = private_dir();
        let o = owner("app", None);
        let rec = {
            let reg = open_at(dir.path(), BUDGET).unwrap();
            let rec = reg.ensure_intent(&o, Q).unwrap();
            reg.mark_proven(&o, [3u8; 32]).unwrap_err();
            rec
        };
        let reg = open_at(dir.path(), BUDGET).unwrap();
        assert_conflict_everywhere(&reg, &o, rec.public_key);
    }

    #[cfg(unix)]
    #[test]
    fn budget_is_persisted_and_a_different_budget_is_refused() {
        let dir = private_dir();
        drop(open_at(dir.path(), BUDGET).unwrap());
        for other in [
            BUDGET + QUOTA_ALIGN_BYTES,
            BUDGET - QUOTA_ALIGN_BYTES,
            2 * BUDGET,
            MIN_QUOTA_BYTES,
        ] {
            assert_eq!(
                open_at(dir.path(), other).unwrap_err(),
                SqldRegistryError::BudgetMismatch,
                "budget {other}"
            );
        }
        assert!(open_at(dir.path(), BUDGET).is_ok());
        // o orçamento é relido a cada escrita: alteração externa fecha o ledger
        let reg = open_at(dir.path(), BUDGET).unwrap();
        raw(dir.path())
            .execute(
                "UPDATE sqld_registry_meta SET managed_budget_bytes = managed_budget_bytes * 2",
                [],
            )
            .unwrap();
        assert_eq!(
            reg.ensure_intent(&owner("a", None), Q).unwrap_err(),
            SqldRegistryError::BudgetMismatch
        );
    }

    // ---------- integridade (fail-closed) ----------

    #[cfg(unix)]
    #[test]
    fn wrong_master_fails_closed_without_token() {
        let dir = private_dir();
        let o = owner("app", None);
        {
            let reg = open_at(dir.path(), BUDGET).unwrap();
            let rec = reg.ensure_intent(&o, Q).unwrap();
            reg.mark_proven(&o, rec.public_key).unwrap();
        }
        // A master é autenticada no `open`: não existe handle com master errada.
        let before = file_hash(&dir.path().join("registry.db"));
        assert_eq!(
            SqldProvisionRegistry::open(
                &dir.path().join("registry.db"),
                Zeroizing::new(OTHER_MASTER),
                BUDGET,
            )
            .unwrap_err(),
            SqldRegistryError::Integrity
        );
        assert_eq!(
            file_hash(&dir.path().join("registry.db")),
            before,
            "wrong-master open must not write"
        );
        // nada foi reparado nem regenerado: a master certa ainda abre
        let right = open_at(dir.path(), BUDGET).unwrap();
        assert_eq!(
            right.get(&o).unwrap().unwrap().state,
            SqldRegistryState::Proven
        );
    }

    #[cfg(unix)]
    #[test]
    fn tampered_rows_fail_closed_with_static_errors() {
        // (rótulo, UPDATE que adultera a linha do dono "victim")
        let cases: &[(&str, &str)] = &[
            (
                "ciphertext",
                "UPDATE sqld_registry SET key_ciphertext = CAST(CASE WHEN hex(substr(key_ciphertext, 1, 1)) = '00' THEN x'01' ELSE x'00' END || substr(key_ciphertext, 2) AS BLOB)",
            ),
            (
                "ciphertext-as-text",
                "UPDATE sqld_registry SET key_ciphertext = 'not a blob'",
            ),
            (
                "ciphertext-last-byte",
                "UPDATE sqld_registry SET key_ciphertext = CAST(substr(key_ciphertext, 1, length(key_ciphertext) - 1) || CASE WHEN hex(substr(key_ciphertext, -1, 1)) = '00' THEN x'01' ELSE x'00' END AS BLOB)",
            ),
            ("nonce", "UPDATE sqld_registry SET key_nonce = x'000000000000000000000000'"),
            ("nonce-short", "UPDATE sqld_registry SET key_nonce = x'00'"),
            ("ciphertext-short", "UPDATE sqld_registry SET key_ciphertext = x'0102'"),
            ("pubkey", "UPDATE sqld_registry SET public_key = zeroblob(32)"),
            ("pubkey-short", "UPDATE sqld_registry SET public_key = x'01'"),
            ("sql-namespace", "UPDATE sqld_registry SET sql_namespace = 'edger_deadbeef'"),
            ("generation", "UPDATE sqld_registry SET generation = 2"),
            ("state-invalid", "UPDATE sqld_registry SET state = 2 WHERE 0"),
            ("quota-unaligned", "UPDATE sqld_registry SET quota_bytes = quota_bytes + 1"),
            ("quota-below-min", "UPDATE sqld_registry SET quota_bytes = 4096"),
            ("rbac-rewrite", "UPDATE sqld_registry SET rbac_tag = 1, rbac_namespace = 'x'"),
        ];
        for (label, sql) in cases {
            let dir = private_dir();
            let victim = owner("victim", None);
            let rec = {
                let reg = open_at(dir.path(), BUDGET).unwrap();
                let rec = reg.ensure_intent(&victim, Q).unwrap();
                reg.mark_proven(&victim, rec.public_key).unwrap();
                rec
            };
            if *label == "state-invalid" {
                // CHECK do schema impede estado inválido: confirma
                assert!(raw(dir.path())
                    .execute("UPDATE sqld_registry SET state = 9", [])
                    .is_err());
                continue;
            }
            raw(dir.path()).execute(sql, []).unwrap();
            let reg = open_at(dir.path(), BUDGET).unwrap();
            let probe = if *label == "rbac-rewrite" {
                owner("victim", Some("x"))
            } else {
                victim
            };
            assert_eq!(
                reg.get(&probe).unwrap_err(),
                SqldRegistryError::Integrity,
                "{label}"
            );
            assert_eq!(
                reg.sign_scoped_token(&probe, now(), 60).unwrap_err(),
                SqldRegistryError::Integrity,
                "{label}"
            );
            assert_eq!(
                reg.mark_proven(&probe, rec.public_key).unwrap_err(),
                SqldRegistryError::Integrity,
                "{label}"
            );
            assert_eq!(
                reg.ensure_intent(&probe, Q).unwrap_err(),
                SqldRegistryError::Integrity,
                "{label}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn owner_rewrite_to_another_worker_fails_closed() {
        let dir = private_dir();
        {
            let reg = open_at(dir.path(), BUDGET).unwrap();
            reg.ensure_intent(&owner("alice", None), Q).unwrap();
        }
        // o atacante renomeia a linha de alice para bob (e ajusta o namespace SQL)
        let bob = owner("bob", None);
        let ns = derive_sql_namespace(&bob);
        raw(dir.path())
            .execute(
                "UPDATE sqld_registry SET worker = 'bob', sql_namespace = ?1",
                params![ns],
            )
            .unwrap();
        let reg = open_at(dir.path(), BUDGET).unwrap();
        assert_eq!(reg.get(&bob).unwrap_err(), SqldRegistryError::Integrity);
        assert_eq!(
            reg.sign_scoped_token(&bob, now(), 60).unwrap_err(),
            SqldRegistryError::Integrity
        );
        // swap de envelopes entre dois donos válidos
        let dir2 = private_dir();
        {
            let reg = open_at(dir2.path(), BUDGET).unwrap();
            reg.ensure_intent(&owner("a", None), Q).unwrap();
            reg.ensure_intent(&owner("b", None), Q).unwrap();
        }
        raw(dir2.path())
            .execute_batch(
                "UPDATE sqld_registry SET key_nonce = (SELECT key_nonce FROM sqld_registry WHERE worker='b'),\
                 key_ciphertext = (SELECT key_ciphertext FROM sqld_registry WHERE worker='b'),\
                 public_key = (SELECT public_key FROM sqld_registry WHERE worker='b') WHERE worker='a'",
            )
            .unwrap();
        let reg = open_at(dir2.path(), BUDGET).unwrap();
        assert_eq!(
            reg.get(&owner("a", None)).unwrap_err(),
            SqldRegistryError::Integrity
        );
        assert!(reg.get(&owner("b", None)).unwrap().is_some());
    }

    #[cfg(unix)]
    #[test]
    fn namespace_collision_with_foreign_row_is_a_conflict_not_a_suffix() {
        let dir = private_dir();
        let reg = open_at(dir.path(), BUDGET).unwrap();
        let o = owner("app", None);
        raw(dir.path())
            .execute(
                "INSERT INTO sqld_registry (worker, rbac_tag, rbac_namespace, sql_namespace, \
                 state, generation, public_key, key_nonce, key_ciphertext, quota_bytes) \
                 VALUES ('intruder', 0, '', ?1, 0, 1, x'00', x'00', x'00', ?2)",
                params![derive_sql_namespace(&o), Q as i64],
            )
            .unwrap();
        assert_eq!(
            reg.ensure_intent(&o, Q).unwrap_err(),
            SqldRegistryError::NamespaceCollision
        );
        let rows: i64 = raw(dir.path())
            .query_row("SELECT count(*) FROM sqld_registry", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[cfg(unix)]
    fn file_hash(path: &Path) -> Vec<u8> {
        Sha256::digest(std::fs::read(path).unwrap()).to_vec()
    }

    #[cfg(unix)]
    #[test]
    fn schema_version_and_foreign_or_corrupt_files_are_rejected_unchanged() {
        // versão desconhecida
        let dir = private_dir();
        drop(open_at(dir.path(), BUDGET).unwrap());
        raw(dir.path())
            .execute("UPDATE sqld_registry_meta SET schema_version = 99", [])
            .unwrap();
        assert_eq!(
            open_at(dir.path(), BUDGET).unwrap_err(),
            SqldRegistryError::UnsupportedSchema
        );

        // schema adulterado (UNIQUE removido): fecha
        let dir = private_dir();
        drop(open_at(dir.path(), BUDGET).unwrap());
        {
            let conn = raw(dir.path());
            conn.execute_batch(
                "PRAGMA writable_schema = ON;\
                 UPDATE sqlite_master SET sql = replace(sql, 'sql_namespace TEXT NOT NULL UNIQUE', 'sql_namespace TEXT NOT NULL') WHERE name = 'sqld_registry';",
            )
            .unwrap();
        }
        assert!(open_at(dir.path(), BUDGET).is_err());

        // SQLite alheio (tabela própria, application_id 0)
        let dir = private_dir();
        let db = dir.path().join("registry.db");
        {
            // cria o arquivo com modo 0600 antes do SQLite
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&db)
                .unwrap();
            Connection::open(&db)
                .unwrap()
                .execute_batch("CREATE TABLE other (x INTEGER); INSERT INTO other VALUES (1);")
                .unwrap();
        }
        let before = file_hash(&db);
        assert_eq!(
            open_at(dir.path(), BUDGET).unwrap_err(),
            SqldRegistryError::ForeignDatabase
        );
        assert_eq!(file_hash(&db), before, "foreign db was modified");

        // application_id certo mas sem tabelas: não autocria
        let dir = private_dir();
        let db = dir.path().join("registry.db");
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&db)
                .unwrap();
            Connection::open(&db)
                .unwrap()
                .execute_batch(&format!("PRAGMA application_id = {APPLICATION_ID};"))
                .unwrap();
        }
        let before = file_hash(&db);
        assert!(open_at(dir.path(), BUDGET).is_err());
        assert_eq!(file_hash(&db), before);

        // lixo não-SQLite
        let dir = private_dir();
        let db = dir.path().join("registry.db");
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&db)
                .unwrap();
            f.write_all(&[0xAB; 4096]).unwrap();
        }
        let before = file_hash(&db);
        assert_eq!(
            open_at(dir.path(), BUDGET).unwrap_err(),
            SqldRegistryError::ForeignDatabase
        );
        assert_eq!(file_hash(&db), before, "garbage file was modified");
    }

    // ---------- concorrência (duas conexões independentes, MESMO arquivo) ----------

    #[cfg(unix)]
    #[test]
    fn two_handles_race_for_the_last_reservation_exactly_one_wins() {
        for round in 0..25 {
            let dir = private_dir();
            let budget = 2 * Q; // 32 MiB
            {
                let seed = open_at(dir.path(), budget).unwrap();
                seed.ensure_intent(&owner("seed", None), Q).unwrap();
            }
            let barrier = Arc::new(Barrier::new(2));
            let handles: Vec<_> = ["racer-a", "racer-b"]
                .into_iter()
                .map(|name| {
                    let path = dir.path().to_path_buf();
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        // handle PRÓPRIO (conexão independente) por thread
                        let reg = open_at(&path, budget).unwrap();
                        barrier.wait();
                        reg.ensure_intent(&owner(name, None), Q)
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let ok = results.iter().filter(|r| r.is_ok()).count();
            let exceeded = results
                .iter()
                .filter(|r| matches!(r, Err(SqldRegistryError::BudgetExceeded)))
                .count();
            assert_eq!((ok, exceeded), (1, 1), "round {round}: {results:?}");
            let conn = raw(dir.path());
            let (sum, rows): (i64, i64) = conn
                .query_row(
                    "SELECT sum(quota_bytes), count(*) FROM sqld_registry",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert!(sum as u64 <= budget);
            assert_eq!(rows, 2);
        }
    }

    #[cfg(unix)]
    #[test]
    fn two_handles_same_owner_yield_one_row_and_one_key() {
        for round in 0..25 {
            let dir = private_dir();
            drop(open_at(dir.path(), BUDGET).unwrap());
            let barrier = Arc::new(Barrier::new(2));
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let path = dir.path().to_path_buf();
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        let reg = open_at(&path, BUDGET).unwrap();
                        barrier.wait();
                        reg.ensure_intent(&owner("same", Some("t")), Q).unwrap()
                    })
                })
                .collect();
            let recs: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            assert_eq!(recs[0], recs[1], "round {round}");
            let rows: i64 = raw(dir.path())
                .query_row("SELECT count(*) FROM sqld_registry", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 1);
            // a única chave gravada é a retornada às duas threads
            let reg = open_at(dir.path(), BUDGET).unwrap();
            assert_eq!(
                reg.get(&owner("same", Some("t"))).unwrap().unwrap(),
                recs[0]
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_first_open_creates_one_consistent_ledger() {
        for _ in 0..10 {
            let dir = private_dir();
            let barrier = Arc::new(Barrier::new(2));
            let handles: Vec<_> = [BUDGET, BUDGET + QUOTA_ALIGN_BYTES]
                .into_iter()
                .map(|budget| {
                    let path = dir.path().to_path_buf();
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        open_at(&path, budget).map(|_| budget)
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            let winners: Vec<u64> = results
                .iter()
                .filter_map(|r| r.as_ref().ok().copied())
                .collect();
            assert_eq!(winners.len(), 1, "{results:?}");
            let stored: i64 = raw(dir.path())
                .query_row(
                    "SELECT managed_budget_bytes FROM sqld_registry_meta",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(stored as u64, winners[0]);
        }
    }

    // ---------- master check: adulteração, schema antigo, dono literal ----------

    /// (public_key, nonce, ciphertext) do check de um ledger criado com `m`.
    #[cfg(unix)]
    fn check_cols_for(m: [u8; MASTER_KEY_LEN]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let dir = private_dir();
        drop(
            SqldProvisionRegistry::open(&dir.path().join("registry.db"), Zeroizing::new(m), BUDGET)
                .unwrap(),
        );
        raw(dir.path())
            .query_row(
                "SELECT check_public_key, check_nonce, check_ciphertext FROM sqld_registry_meta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }

    #[cfg(unix)]
    fn registry_rows(dir: &Path) -> (i64, i64) {
        raw(dir)
            .query_row(
                "SELECT count(*), coalesce(sum(quota_bytes), 0) FROM sqld_registry",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn tampered_master_check_fails_closed_at_open_and_on_live_handles() {
        let (other_pub, other_nonce, other_ct) = check_cols_for(OTHER_MASTER);
        let tampers: Vec<(&str, String)> = vec![
            (
                "ciphertext bit flip",
                "UPDATE sqld_registry_meta SET check_ciphertext = CAST(\
                 CASE WHEN substr(check_ciphertext, 1, 1) = x'00' THEN x'01' ELSE x'00' END \
                 || substr(check_ciphertext, 2) AS BLOB)"
                    .to_string(),
            ),
            (
                "nonce replaced",
                "UPDATE sqld_registry_meta SET check_nonce = zeroblob(12)".to_string(),
            ),
            (
                "public key replaced",
                "UPDATE sqld_registry_meta SET check_public_key = zeroblob(32)".to_string(),
            ),
            (
                "public key wrong length",
                "UPDATE sqld_registry_meta SET check_public_key = x'00'".to_string(),
            ),
            (
                "ciphertext empty",
                "UPDATE sqld_registry_meta SET check_ciphertext = x''".to_string(),
            ),
            (
                "column type text",
                "UPDATE sqld_registry_meta SET check_nonce = 'text'".to_string(),
            ),
        ];
        let o = owner("app", None);
        let newcomer = owner("newcomer", None);
        for (case, sql) in tampers {
            let dir = private_dir();
            let live = open_at(dir.path(), BUDGET).unwrap();
            let rec = live.ensure_intent(&o, Q).unwrap();
            raw(dir.path()).execute(&sql, []).unwrap();
            // handle já aberto: toda operação falha fechada, nada novo entra
            assert_eq!(
                live.get(&o).unwrap_err(),
                SqldRegistryError::Integrity,
                "{case}"
            );
            assert_eq!(
                live.ensure_intent(&newcomer, Q).unwrap_err(),
                SqldRegistryError::Integrity,
                "{case}"
            );
            assert_eq!(registry_rows(dir.path()), (1, Q as i64), "{case}");
            // open novo (a master certa): também recusa, sem reparar o check
            assert_eq!(
                open_at(dir.path(), BUDGET).map(|_| ()).unwrap_err(),
                SqldRegistryError::Integrity,
                "{case}"
            );
            let _ = rec;
        }
        // check trocado pelo de um ledger de OUTRA master: um handle vivo da
        // master antiga também recusa (não admite dono novo).
        let dir = private_dir();
        let live = open_at(dir.path(), BUDGET).unwrap();
        live.ensure_intent(&o, Q).unwrap();
        raw(dir.path())
            .execute(
                "UPDATE sqld_registry_meta SET check_public_key = ?1, check_nonce = ?2, \
                 check_ciphertext = ?3",
                params![other_pub, other_nonce, other_ct],
            )
            .unwrap();
        assert_eq!(
            live.ensure_intent(&newcomer, Q).unwrap_err(),
            SqldRegistryError::Integrity
        );
        assert_eq!(registry_rows(dir.path()), (1, Q as i64));
    }

    #[cfg(unix)]
    #[test]
    fn master_check_owner_literal_is_not_a_registry_row_and_costs_no_quota() {
        let dir = private_dir();
        let reg = open_at(dir.path(), BUDGET).unwrap();
        // ledger recém-criado: o check não é linha nem reserva
        assert_eq!(registry_rows(dir.path()), (0, 0));
        // um dono real com o mesmo literal é só mais um dono: linha e quota
        // próprias, e o check continua valendo.
        let lit = owner(MASTER_CHECK_WORKER, None);
        let rec = reg.ensure_intent(&lit, Q).unwrap();
        assert_eq!(registry_rows(dir.path()), (1, Q as i64));
        assert_eq!(reg.get(&lit).unwrap().unwrap(), rec);
        drop(reg);
        let reg = open_at(dir.path(), BUDGET).unwrap();
        assert_eq!(reg.get(&lit).unwrap().unwrap(), rec);
        assert_eq!(
            SqldProvisionRegistry::open(
                &dir.path().join("registry.db"),
                Zeroizing::new(OTHER_MASTER),
                BUDGET
            )
            .map(|_| ())
            .unwrap_err(),
            SqldRegistryError::Integrity
        );
    }

    #[cfg(unix)]
    #[test]
    fn old_v1_ledger_without_master_check_is_rejected_unchanged() {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = private_dir();
        let db = dir.path().join("registry.db");
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&db)
            .unwrap();
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(&format!(
                "CREATE TABLE sqld_registry_meta (\
                 id INTEGER PRIMARY KEY CHECK (id = 1),\
                 schema_version INTEGER NOT NULL,\
                 managed_budget_bytes INTEGER NOT NULL CHECK (managed_budget_bytes > 0));\
                 {REGISTRY_SQL};\
                 PRAGMA application_id = {APPLICATION_ID};\
                 INSERT INTO sqld_registry_meta VALUES (1, 1, {BUDGET});"
            ))
            .unwrap();
        }
        let before = file_hash(&db);
        assert_eq!(
            open_at(dir.path(), BUDGET).map(|_| ()).unwrap_err(),
            SqldRegistryError::UnsupportedSchema
        );
        assert_eq!(file_hash(&db), before, "old ledger was modified");
    }

    // R38-BEGIN (só API pública + helpers antigos: roda também contra a fonte antiga)
    #[cfg(unix)]
    mod r38 {
        use super::*;
        use std::process::{Command, Stdio};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};

        const CHILD_DIR: &str = "EDGER_R38_CHILD_DIR";
        const CHILD_BUSY: i32 = 0;
        const CHILD_ACQUIRED: i32 = 7;

        fn child_dir() -> Option<std::path::PathBuf> {
            std::env::var_os(CHILD_DIR).map(std::path::PathBuf::from)
        }

        /// Roda um teste `#[ignore]` deste módulo num processo filho
        /// independente (o binário de teste atual), com ambiente mínimo e
        /// prazo: no estouro, mata e colhe (`kill` + `wait`).
        fn run_child(test: &str, dir: &Path) -> i32 {
            let full = format!(
                "{}::{test}",
                module_path!()
                    .split_once("::")
                    .map(|(_, rest)| rest)
                    .unwrap()
            );
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([full.as_str(), "--exact", "--ignored", "--test-threads=1"])
                .env_clear()
                .env(CHILD_DIR, dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                match child.try_wait().unwrap() {
                    Some(status) => return status.code().unwrap_or(-1),
                    None if Instant::now() >= deadline => {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("child {test} timed out and was killed");
                    }
                    None => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        }

        fn entries(dir: &Path) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        // ---- :memory: / URI em modo persistente ----

        /// Filho: cwd privado próprio (o processo do filho é só dele; o
        /// processo do teste nunca muda de cwd).
        #[test]
        #[ignore = "helper: roda só como processo filho de relative_special_names_..."]
        fn child_relative_special_names() {
            let Some(dir) = child_dir() else { return };
            std::env::set_current_dir(&dir).unwrap();
            for name in [
                ":memory:",
                "./:memory:",
                "file:x?mode=memory",
                "file::memory:",
            ] {
                let res = SqldProvisionRegistry::open(Path::new(name), master(), BUDGET);
                assert_eq!(
                    res.map(|_| ()).unwrap_err(),
                    SqldRegistryError::UnsafePath,
                    "{name}"
                );
            }
            // um nome relativo normal persiste de verdade: dois opens, mesma
            // identidade/namespace/quota.
            let o = owner("app", None);
            let first = {
                let reg =
                    SqldProvisionRegistry::open(Path::new("ledger.db"), master(), BUDGET).unwrap();
                reg.ensure_intent(&o, Q).unwrap()
            };
            let reg =
                SqldProvisionRegistry::open(Path::new("ledger.db"), master(), BUDGET).unwrap();
            let again = reg.get(&o).unwrap().unwrap();
            assert_eq!(again.sql_namespace, first.sql_namespace);
            assert_eq!(again.public_key, first.public_key);
            assert_eq!(again.quota_bytes, Q);
            // a quota persistida conta: o orçamento não aceita mais que BUDGET
            assert_eq!(
                reg.ensure_intent(&owner("big", None), BUDGET).unwrap_err(),
                SqldRegistryError::BudgetExceeded
            );
        }

        #[test]
        fn relative_special_names_are_refused_without_creating_a_file_and_normal_names_persist() {
            let dir = private_dir();
            assert_eq!(run_child("child_relative_special_names", dir.path()), 0);
            // nenhum arquivo órfão: só o ledger real (e nada de `:memory:`)
            assert_eq!(entries(dir.path()), vec!["ledger.db".to_string()]);
        }

        /// Filho: `file:private/ledger.db` tem basename `ledger.db` (passa no
        /// filtro de basename), mas o SQLite recebe o caminho inteiro e, com
        /// URI habilitada, abriria `private/ledger.db`. Duas pastas privadas
        /// 0700 (`file:private` e `private`) no cwd do filho.
        #[test]
        #[ignore = "helper: roda só como processo filho de full_path_file_prefix_..."]
        fn child_full_path_file_prefix() {
            use std::os::unix::fs::PermissionsExt;
            let Some(dir) = child_dir() else { return };
            std::env::set_current_dir(&dir).unwrap();
            for d in ["file:private", "private"] {
                std::fs::create_dir(d).unwrap();
                std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            for name in [
                "file:private/ledger.db",
                "file:private/sub/ledger.db",
                "file:/ledger.db",
            ] {
                let res = SqldProvisionRegistry::open(Path::new(name), master(), BUDGET);
                assert_eq!(
                    res.map(|_| ()).unwrap_err(),
                    SqldRegistryError::UnsafePath,
                    "{name}"
                );
            }
            // nada foi criado/tocado em nenhuma das duas pastas
            for d in ["file:private", "private"] {
                assert_eq!(std::fs::read_dir(d).unwrap().count(), 0, "{d}");
            }
            // controle: `private/ledger.db` (sem prefixo `file:`) persiste
            drop(
                SqldProvisionRegistry::open(Path::new("private/ledger.db"), master(), BUDGET)
                    .unwrap(),
            );
            assert_eq!(std::fs::read_dir("private").unwrap().count(), 1);
            assert_eq!(std::fs::read_dir("file:private").unwrap().count(), 0);
        }

        #[test]
        fn full_path_file_prefix_is_refused_without_touching_any_folder() {
            let dir = private_dir();
            assert_eq!(run_child("child_full_path_file_prefix", dir.path()), 0);
            assert_eq!(
                entries(dir.path()),
                vec!["file:private".to_string(), "private".to_string()]
            );
        }

        #[test]
        fn absolute_special_basenames_are_refused_consistently_without_a_file() {
            let dir = private_dir();
            for name in [":memory:", "file:x?mode=memory", "file:abs"] {
                let err = SqldProvisionRegistry::open(&dir.path().join(name), master(), BUDGET)
                    .map(|_| ())
                    .unwrap_err();
                assert_eq!(err, SqldRegistryError::UnsafePath, "{name}");
            }
            assert!(entries(dir.path()).is_empty(), "{:?}", entries(dir.path()));
        }

        // ---- master autenticada antes de qualquer write/admit ----

        #[test]
        fn wrong_master_cannot_open_or_admit_a_new_owner_and_correct_master_is_intact() {
            let dir = private_dir();
            let a = owner("app-a", None);
            let b = owner("app-b", Some("ns-b"));
            let rec_a = {
                let reg = open_at(dir.path(), BUDGET).unwrap();
                reg.ensure_intent(&a, Q).unwrap()
            };
            let db = dir.path().join("registry.db");
            let before = file_hash(&db);
            let wrong = SqldProvisionRegistry::open(&db, Zeroizing::new(OTHER_MASTER), BUDGET);
            // se algum handle de master errada existisse, tentaria admitir B
            let admitted = wrong.as_ref().ok().map(|w| w.ensure_intent(&b, Q).is_ok());
            assert_eq!(admitted, None, "wrong master got a handle / admitted B");
            assert_eq!(wrong.map(|_| ()).unwrap_err(), SqldRegistryError::Integrity);
            assert_eq!(file_hash(&db), before, "wrong master touched the ledger");
            let rows: (i64, i64) = raw(dir.path())
                .query_row(
                    "SELECT count(*), sum(quota_bytes) FROM sqld_registry",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(rows, (1, Q as i64));
            // a master certa abre tudo intacto e ainda admite B
            let right = open_at(dir.path(), BUDGET).unwrap();
            assert_eq!(right.get(&a).unwrap().unwrap(), rec_a);
            assert_eq!(right.get(&b).unwrap(), None);
            right.ensure_intent(&b, Q).unwrap();
        }

        #[test]
        fn wrong_master_on_a_ledger_without_apps_is_also_refused() {
            let dir = private_dir();
            drop(open_at(dir.path(), BUDGET).unwrap());
            let db = dir.path().join("registry.db");
            let before = file_hash(&db);
            let wrong = SqldProvisionRegistry::open(&db, Zeroizing::new(OTHER_MASTER), BUDGET);
            let admitted = wrong
                .as_ref()
                .ok()
                .map(|w| w.ensure_intent(&owner("first", None), Q).is_ok());
            assert_eq!(admitted, None, "wrong master adopted an empty ledger");
            assert_eq!(wrong.map(|_| ()).unwrap_err(), SqldRegistryError::Integrity);
            assert_eq!(file_hash(&db), before);
            // a master certa ainda adota o ledger vazio
            open_at(dir.path(), BUDGET)
                .unwrap()
                .ensure_intent(&owner("first", None), Q)
                .unwrap();
        }

        // ---- locks POSIX: um segundo handle não derruba o lock do primeiro ----

        /// Filho: tenta o lock de escrita do SQLite no arquivo (sem registro).
        #[test]
        #[ignore = "helper: roda só como processo filho do teste de locks POSIX"]
        fn child_try_write_lock() {
            let Some(dir) = child_dir() else { return };
            let conn = Connection::open_with_flags(
                dir.join("registry.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .unwrap();
            conn.busy_timeout(Duration::from_millis(0)).unwrap();
            match conn.execute_batch("BEGIN IMMEDIATE") {
                Ok(()) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    drop(conn);
                    std::process::exit(CHILD_ACQUIRED);
                }
                Err(err) => {
                    let busy = matches!(
                        err.sqlite_error_code(),
                        Some(
                            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                        )
                    );
                    drop(conn);
                    std::process::exit(if busy { CHILD_BUSY } else { 8 });
                }
            }
        }

        #[derive(Clone, Copy, Debug)]
        enum Second {
            /// reabre o MESMO caminho com sucesso (preflight abre+fecha um fd)
            ReopenOk,
            /// caminho que falha DEPOIS de abrir o fd do arquivo (nlink != 1)
            FailAfterFdOpen,
            /// controle: só derruba (Drop) um handle já aberto
            DropOpenedHandle,
        }

        fn hold_write_lock_while_second_handle_runs(second: Second) {
            let dir = private_dir();
            let a = open_at(dir.path(), BUDGET).unwrap();
            let db = dir.path().join("registry.db");
            let alias = dir.path().join("alias.db");
            let spare = match second {
                Second::DropOpenedHandle => Some(open_at(dir.path(), BUDGET).unwrap()),
                _ => None,
            };
            if matches!(second, Second::FailAfterFdOpen) {
                std::fs::hard_link(&db, &alias).unwrap();
            }
            let finished = Arc::new(AtomicBool::new(false));
            let (path, done) = (
                if matches!(second, Second::FailAfterFdOpen) {
                    alias.clone()
                } else {
                    db.clone()
                },
                Arc::clone(&finished),
            );
            let during = a.with_tx(|tx| {
                // lock de escrita (RESERVED) real, com um UPDATE de fato
                tx.execute(
                    "UPDATE sqld_registry_meta SET schema_version = schema_version",
                    [],
                )
                .map_err(storage)?;
                let worker = std::thread::spawn(move || {
                    let out = match second {
                        Second::DropOpenedHandle => {
                            drop(spare);
                            None
                        }
                        _ => Some(SqldProvisionRegistry::open(&path, master(), BUDGET).map(|_| ())),
                    };
                    done.store(true, Ordering::SeqCst);
                    out
                });
                // dá tempo ao segundo handle de rodar o preflight (sem gate,
                // ele fecha o fd e cancela o lock de A neste intervalo)
                std::thread::sleep(Duration::from_millis(400));
                let code = run_child("child_try_write_lock", dir.path());
                let early = finished.load(Ordering::SeqCst);
                Ok((code, early, worker))
            });
            let (code, early, worker) = during.unwrap();
            let outcome = worker.join().unwrap();
            assert_eq!(
                code, CHILD_BUSY,
                "{second:?}: a child process took the write lock while handle A held it"
            );
            assert!(
                !early,
                "{second:?}: second handle ran inside A's write transaction"
            );
            match second {
                Second::ReopenOk => assert_eq!(outcome, Some(Ok(()))),
                Second::FailAfterFdOpen => {
                    assert_eq!(outcome, Some(Err(SqldRegistryError::UnsafePath)))
                }
                Second::DropOpenedHandle => assert_eq!(outcome, None),
            }
            // controle positivo: com A ocioso o filho CONSEGUE o lock (o
            // sonda não é sempre "busy").
            assert_eq!(
                run_child("child_try_write_lock", dir.path()),
                CHILD_ACQUIRED
            );
        }

        #[test]
        fn second_handle_reopen_cannot_cancel_the_first_handles_posix_write_lock() {
            hold_write_lock_while_second_handle_runs(Second::ReopenOk);
        }

        #[test]
        fn second_handle_failing_after_fd_open_cannot_cancel_the_first_handles_posix_write_lock() {
            hold_write_lock_while_second_handle_runs(Second::FailAfterFdOpen);
        }

        #[test]
        fn dropping_another_handle_while_the_first_writes_is_serialized_and_keeps_the_lock() {
            hold_write_lock_while_second_handle_runs(Second::DropOpenedHandle);
        }
    }
    // R38-END

    // ---------- arquivo seguro (Linux/Unix) ----------

    #[cfg(unix)]
    mod fs_tests {
        use super::*;
        use std::os::unix::fs::{symlink, MetadataExt, OpenOptionsExt, PermissionsExt};

        fn mode(path: &Path) -> u32 {
            std::fs::symlink_metadata(path)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        }

        fn chmod(path: &Path, m: u32) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(m)).unwrap();
        }

        fn entries_of(dir: &Path) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        #[test]
        fn creates_db_0600_in_existing_0700_dir_with_delete_journal_and_full_sync() {
            let dir = private_dir();
            assert_eq!(mode(dir.path()), 0o700);
            let reg = open_at(dir.path(), BUDGET).unwrap();
            let db = dir.path().join("registry.db");
            let md = std::fs::metadata(&db).unwrap();
            assert_eq!(md.permissions().mode() & 0o7777, 0o600);
            assert_eq!(md.nlink(), 1);
            assert_eq!(mode(dir.path()), 0o700, "parent mode must not change");
            reg.with_conn(|conn| {
                let jm: String = conn
                    .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                    .unwrap();
                let sync: i64 = conn
                    .query_row("PRAGMA synchronous", [], |r| r.get(0))
                    .unwrap();
                assert_eq!((jm.as_str(), sync), ("delete", 2));
            });
            for sfx in ["-wal", "-shm"] {
                assert!(!dir.path().join(format!("registry.db{sfx}")).exists());
            }
            // reabrir não muda nada
            drop(reg);
            let _ = open_at(dir.path(), BUDGET).unwrap();
            assert_eq!(mode(&db), 0o600);
            assert_eq!(mode(dir.path()), 0o700);
        }

        #[test]
        fn rollback_journal_is_0600_while_a_write_is_in_flight_and_gone_after() {
            let dir = private_dir();
            let reg = open_at(dir.path(), BUDGET).unwrap();
            reg.ensure_intent(&owner("app", None), Q).unwrap();
            let journal = dir.path().join("registry.db-journal");
            // `Err` força o rollback ao final de `with_tx`.
            let out: Result<(), _> = reg.with_tx(|tx| {
                tx.execute("UPDATE sqld_registry SET state = 1", [])
                    .unwrap();
                assert!(journal.exists(), "journal should exist during a write");
                let md = std::fs::symlink_metadata(&journal).unwrap();
                assert_eq!(md.permissions().mode() & 0o7777, 0o600);
                assert_eq!(md.nlink(), 1);
                Err(SqldRegistryError::Storage)
            });
            assert_eq!(out.unwrap_err(), SqldRegistryError::Storage);
            assert!(!journal.exists());
        }

        #[test]
        fn weak_parent_permissions_are_refused_and_not_changed() {
            for weak in [0o755, 0o750, 0o770, 0o777, 0o701, 0o600, 0o500, 0o1700] {
                let dir = private_dir();
                chmod(dir.path(), weak);
                assert_eq!(
                    open_at(dir.path(), BUDGET).unwrap_err(),
                    SqldRegistryError::WeakPermissions,
                    "mode {weak:o}"
                );
                assert_eq!(mode(dir.path()), weak, "refusal must not chmod");
                assert!(!dir.path().join("registry.db").exists(), "no db created");
                chmod(dir.path(), 0o700);
            }
        }

        #[test]
        fn foreign_owner_parent_and_leaf_are_refused() {
            if rustix::process::geteuid().as_raw() != 0 {
                // Sem root não há como criar um arquivo de outro dono; o ramo de
                // dono errado fica coberto apenas quando o teste roda como root.
                return;
            }
            // Restaura o dono (root) mesmo se um assert falhar: o gate roda sem
            // DAC override/read-search, então root não atravessa nem lê o que
            // pertence a outro UID; a inspeção final acontece só depois da
            // restauração controlada (chown precisa só de CAP_CHOWN) ou por fd
            // aberto antes de transferir o dono.
            struct RestoreOwner<'a>(&'a Path);
            impl Drop for RestoreOwner<'_> {
                fn drop(&mut self) {
                    let _ = std::os::unix::fs::chown(self.0, Some(0), None);
                }
            }
            use std::io::{Read, Seek, SeekFrom};
            let hash_fd = |mut f: &std::fs::File| {
                let mut buf = Vec::new();
                f.seek(SeekFrom::Start(0)).unwrap();
                f.read_to_end(&mut buf).unwrap();
                Sha256::digest(buf).to_vec()
            };

            // Caso 1: parent estrangeiro (0700, dono 12345).
            let dir = private_dir();
            {
                let _restore = RestoreOwner(dir.path());
                std::os::unix::fs::chown(dir.path(), Some(12345), None).unwrap();
                assert_eq!(
                    open_at(dir.path(), BUDGET).unwrap_err(),
                    SqldRegistryError::WeakPermissions
                );
            }
            // dono restaurado: agora dá para inspecionar o alvo sem DAC.
            assert_eq!(std::fs::metadata(dir.path()).unwrap().uid(), 0);
            assert_eq!(mode(dir.path()), 0o700, "parent mode must not change");
            assert!(
                entries_of(dir.path()).is_empty(),
                "refused parent must stay empty"
            );

            // Caso 2: leaf estrangeiro (parent root 0700, ledger 0600 do 12345).
            drop(open_at(dir.path(), BUDGET).unwrap());
            let db = dir.path().join("registry.db");
            let fd = std::fs::File::open(&db).unwrap(); // antes da troca de dono
            let before_hash = hash_fd(&fd);
            let before = fd.metadata().unwrap();
            {
                let _restore = RestoreOwner(&db);
                std::os::unix::fs::chown(&db, Some(12345), None).unwrap();
                assert_eq!(
                    open_at(dir.path(), BUDGET).unwrap_err(),
                    SqldRegistryError::WeakPermissions
                );
                // por fd (sem reabrir por caminho): conteúdo, inode, dono e modo
                let after = fd.metadata().unwrap();
                assert_eq!(hash_fd(&fd), before_hash);
                assert_eq!((after.ino(), after.size()), (before.ino(), before.size()));
                assert_eq!(after.uid(), 12345, "module must not chown the leaf");
                assert_eq!(after.permissions().mode() & 0o7777, 0o600);
            }
            // dono restaurado: o caminho ainda aponta ao mesmo arquivo e bytes.
            assert_eq!(file_hash(&db), before_hash);
            assert_eq!(std::fs::metadata(&db).unwrap().ino(), before.ino());
            assert_eq!(entries_of(dir.path()), vec!["registry.db".to_string()]);
        }

        #[test]
        fn weak_leaf_modes_are_refused_without_touching_the_file() {
            for weak in [0o644, 0o640, 0o660, 0o666, 0o400, 0o700, 0o4600] {
                let dir = private_dir();
                drop(open_at(dir.path(), BUDGET).unwrap());
                let db = dir.path().join("registry.db");
                chmod(&db, weak);
                let before = file_hash(&db);
                assert_eq!(
                    open_at(dir.path(), BUDGET).unwrap_err(),
                    SqldRegistryError::WeakPermissions,
                    "mode {weak:o}"
                );
                assert_eq!(mode(&db), weak, "refusal must not chmod");
                assert_eq!(file_hash(&db), before);
            }
        }

        #[test]
        fn parent_symlink_is_refused_and_target_untouched() {
            let real = private_dir();
            let holder = private_dir();
            let link = holder.path().join("link");
            symlink(real.path(), &link).unwrap();
            let err = SqldProvisionRegistry::open(&link.join("registry.db"), master(), BUDGET)
                .unwrap_err();
            assert_eq!(err, SqldRegistryError::UnsafePath);
            assert_eq!(
                std::fs::read_dir(real.path()).unwrap().count(),
                0,
                "target touched"
            );
            assert_eq!(mode(real.path()), 0o700);
        }

        #[test]
        fn leaf_symlink_is_refused_and_target_untouched() {
            let dir = private_dir();
            let target = dir.path().join("target.bin");
            {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&target)
                    .unwrap();
            }
            symlink(&target, dir.path().join("registry.db")).unwrap();
            assert_eq!(
                open_at(dir.path(), BUDGET).unwrap_err(),
                SqldRegistryError::UnsafePath
            );
            assert_eq!(
                std::fs::metadata(&target).unwrap().len(),
                0,
                "target written"
            );
            // symlink pendurado (dangling) também recusa e não cria o alvo
            let dir = private_dir();
            let ghost = dir.path().join("ghost.db");
            symlink(&ghost, dir.path().join("registry.db")).unwrap();
            assert_eq!(
                open_at(dir.path(), BUDGET).unwrap_err(),
                SqldRegistryError::UnsafePath
            );
            assert!(!ghost.exists(), "dangling target created");
        }

        #[test]
        fn leaf_hardlink_and_non_regular_leaf_are_refused() {
            let dir = private_dir();
            drop(open_at(dir.path(), BUDGET).unwrap());
            let db = dir.path().join("registry.db");
            std::fs::hard_link(&db, dir.path().join("alias.db")).unwrap();
            let before = file_hash(&db);
            assert_eq!(
                open_at(dir.path(), BUDGET).unwrap_err(),
                SqldRegistryError::UnsafePath
            );
            assert_eq!(file_hash(&db), before);
            // diretório no lugar do arquivo
            let dir = private_dir();
            std::fs::create_dir(dir.path().join("registry.db")).unwrap();
            assert!(open_at(dir.path(), BUDGET).is_err());
        }

        #[test]
        fn missing_parent_is_not_created_and_bad_leaf_names_are_refused() {
            let dir = private_dir();
            let missing = dir.path().join("nope");
            assert_eq!(
                SqldProvisionRegistry::open(&missing.join("registry.db"), master(), BUDGET)
                    .unwrap_err(),
                SqldRegistryError::UnsafePath
            );
            assert!(!missing.exists(), "parent must not be created");
            for bad in [Path::new("/"), Path::new(""), &dir.path().join("..")] {
                assert!(SqldProvisionRegistry::open(bad, master(), BUDGET).is_err());
            }
        }

        #[test]
        fn sidecars_are_checked_wal_refused_and_journal_must_be_private() {
            for sfx in ["-wal", "-shm"] {
                let dir = private_dir();
                drop(open_at(dir.path(), BUDGET).unwrap());
                std::fs::write(dir.path().join(format!("registry.db{sfx}")), b"x").unwrap();
                assert_eq!(
                    open_at(dir.path(), BUDGET).unwrap_err(),
                    SqldRegistryError::UnsafePath
                );
            }
            // journal com modo fraco
            let dir = private_dir();
            drop(open_at(dir.path(), BUDGET).unwrap());
            let journal = dir.path().join("registry.db-journal");
            std::fs::write(&journal, b"").unwrap();
            chmod(&journal, 0o644);
            assert_eq!(
                open_at(dir.path(), BUDGET).unwrap_err(),
                SqldRegistryError::WeakPermissions
            );
            // journal symlink
            std::fs::remove_file(&journal).unwrap();
            let decoy = dir.path().join("decoy");
            std::fs::write(&decoy, b"keep").unwrap();
            symlink(&decoy, &journal).unwrap();
            assert!(open_at(dir.path(), BUDGET).is_err());
            assert_eq!(std::fs::read(&decoy).unwrap(), b"keep");
        }

        #[test]
        fn wal_mode_database_is_refused() {
            let dir = private_dir();
            drop(open_at(dir.path(), BUDGET).unwrap());
            raw(dir.path())
                .query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
                .unwrap();
            // sai do WAL sem checkpoint? a conexão acima fechou e removeu -wal/-shm;
            // o modo WAL persiste no cabeçalho.
            assert_eq!(
                open_at(dir.path(), BUDGET).unwrap_err(),
                SqldRegistryError::UnsafeJournalMode
            );
        }

        #[test]
        fn open_errors_never_echo_the_path() {
            let dir = private_dir();
            chmod(dir.path(), 0o755);
            let err = open_at(dir.path(), BUDGET).unwrap_err();
            let shown = format!("{err:?} {err}");
            assert!(!shown.contains(dir.path().to_str().unwrap()));
            assert!(!shown.contains("registry.db"));
            chmod(dir.path(), 0o700);
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn open_is_unsupported_off_unix() {
        let err =
            SqldProvisionRegistry::open(Path::new("registry.db"), master(), BUDGET).unwrap_err();
        assert_eq!(err, SqldRegistryError::Unsupported);
    }
}
