//! EDG-54 (15e): aplicação PURA da configuração de banco gerenciado a uma
//! cópia do `WorkerConfig` de uma versão instalada.
//!
//! Sem I/O, sem relógio, sem env e sem cofre: recebe um [`WorkerRef`] confiável
//! (já resolvido pelo host a partir do índice) e a origem de dados do host, e
//! devolve um CLONE do `WorkerConfig`. O `WorkerRef`, o manifest e o índice
//! nunca mudam. A função não resolve o valor de nenhum segredo e não cria
//! grant: só põe `SQLD_URL` no env (quando ausente/vazio) e o NOME
//! `SQLD_TOKEN` em `secrets` (sem duplicar). O valor do token nunca passa por
//! aqui. Quem a chama (a fábrica, em fatia futura) usa a cópia só no spawn.
//!
//! # Conflitos estáticos (negam o vínculo; nada é sobrescrito)
//!
//! * `SQLD_URL` no env não vazia e que NÃO seja a origem raiz normalizada do
//!   host (inclui userinfo/path/query/fragment, esquema ou host diferentes,
//!   valor inválido ou com espaço/controle nas bordas);
//! * `SQLD_TOKEN` no env (qualquer valor, inclusive vazio) ou em `publicEnv`
//!   (o nome do `publicEnv` é comparado com `str::trim`, como o consumidor);
//! * `SQLD_URL` em `secrets` (o cofre sobrescreveria o env inserido);
//! * runtime Deno (backend de processo) sem `allowNet: []` explícito: `None`
//!   ou lista não vazia são recusados. WASM, fullstack WASM e SPA não ganham
//!   rede e não exigem `allowNet`.
//!
//! O modo Deno vem de [`ExecutionKind::uses_process_backend`], o mesmo predicado
//! do `ExecutionKind` que a fábrica do binário usa para separar o backend WASM
//! (`WasmModule` e `fullstack` + `adapter: wasm`) do restante; `StaticSpa`
//! também não usa processo. Não há parser de `kind` novo.
//!
//! Limite: `envPrefix` (prefixos de variáveis do host repassadas) NÃO é
//! examinado aqui; o contrato não o trata como conflito.

use std::fmt;

use edger_core::{WorkerConfig, WorkerRef};
use reqwest::Url;

use crate::sqld_provision::SQLD_TOKEN_SECRET;

/// Nome do env público (não secreto) com a origem de dados.
pub const SQLD_URL_ENV: &str = "SQLD_URL";

/// Conflito estático: nunca carrega valores, URLs nem nomes de worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedDatabaseConfigError {
    /// A origem do host não é uma origem raiz `http(s)://host[:porta]/`.
    InvalidHostOrigin,
    /// `SQLD_URL` no env difere da origem do host (ou é inválida).
    UrlConflict,
    /// `SQLD_URL` declarado em `secrets`.
    UrlSecretConflict,
    /// `SQLD_TOKEN` no env.
    TokenEnvConflict,
    /// `SQLD_TOKEN` em `publicEnv`.
    TokenPublicEnvConflict,
    /// Deno sem `allowNet: []` explícito.
    DenoAllowNetRequired,
}

impl fmt::Display for ManagedDatabaseConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidHostOrigin => "managed database: invalid host data origin",
            Self::UrlConflict => "managed database: SQLD_URL conflicts with the host origin",
            Self::UrlSecretConflict => "managed database: SQLD_URL is declared as a secret",
            Self::TokenEnvConflict => "managed database: SQLD_TOKEN is set in env",
            Self::TokenPublicEnvConflict => "managed database: SQLD_TOKEN is in publicEnv",
            Self::DenoAllowNetRequired => "managed database: Deno requires an empty allowNet",
        })
    }
}

impl std::error::Error for ManagedDatabaseConfigError {}

/// Normaliza uma origem RAIZ: `http`/`https`, host, sem userinfo, sem
/// path (além de `/`), query ou fragmento, e sem espaço/controle no texto.
fn normalize_root_origin(value: &str) -> Option<String> {
    if value.is_empty() || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return None;
    }
    Some(url.as_str().to_owned())
}

/// Devolve o clone do `WorkerConfig` com o banco gerenciado aplicado, ou o
/// conflito estático que nega o vínculo. Pura: ver o topo do módulo.
pub fn apply_managed_database_config(
    worker: &WorkerRef,
    host_origin: &str,
) -> Result<WorkerConfig, ManagedDatabaseConfigError> {
    use ManagedDatabaseConfigError as E;
    let host = normalize_root_origin(host_origin).ok_or(E::InvalidHostOrigin)?;
    let config = &worker.config;

    if config.env.contains_key(SQLD_TOKEN_SECRET) {
        return Err(E::TokenEnvConflict);
    }
    if config
        .public_env
        .iter()
        .any(|name| name.trim() == SQLD_TOKEN_SECRET)
    {
        return Err(E::TokenPublicEnvConflict);
    }
    if config.secrets.iter().any(|name| name == SQLD_URL_ENV) {
        return Err(E::UrlSecretConflict);
    }
    let set_url = match config.env.get(SQLD_URL_ENV) {
        None => true,
        Some(value) if value.is_empty() => true,
        Some(value) => {
            if normalize_root_origin(value).as_deref() != Some(host.as_str()) {
                return Err(E::UrlConflict);
            }
            false
        }
    };
    if worker.kind.uses_process_backend()
        && !matches!(&config.allow_net, Some(list) if list.is_empty())
    {
        return Err(E::DenoAllowNetRequired);
    }

    let mut out = config.clone();
    if set_url {
        out.env.insert(SQLD_URL_ENV.to_owned(), host);
    }
    if !out.secrets.iter().any(|name| name == SQLD_TOKEN_SECRET) {
        out.secrets.push(SQLD_TOKEN_SECRET.to_owned());
    }
    Ok(out)
}
