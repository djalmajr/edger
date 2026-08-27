//! Admin API vocabulary. Pure response/request shapes only.

use crate::manifest::WorkerVisibility;
use crate::{ApiKeyPrincipal, ExecutionKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminErrorResponse {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminCatalogItem {
    pub id: String,
    pub kind: String,
    pub owner: String,
    pub owner_kind: String,
    pub route: String,
    pub source: String,
    pub status: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminCatalogResponse {
    pub items: Vec<AdminCatalogItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminMutationResponse {
    pub code: String,
    pub message: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminSessionResponse {
    pub principal: ApiKeyPrincipal,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkerOrigin {
    CoreBundled,
    CoreOverlay,
    User,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminWorkerInfo {
    pub kind: ExecutionKind,
    pub name: String,
    pub namespace: Option<String>,
    pub origin: WorkerOrigin,
    pub plugin_base: Option<String>,
    pub source: String,
    pub status: String,
    pub version: String,
    pub visibility: WorkerVisibility,
    /// Revisão CAS do diretório instalado; None para instalações pré-rastreio.
    pub revision: Option<String>,
    /// Versão pública instalada apenas para acesso versionado até promote.
    pub staged: bool,
    /// Ponteiro de promoção EXPLÍCITO do worker (mesmo valor em todas as
    /// versões do name; None = sem promote, o roteador cai na maior semver
    /// pública). É o que permite ao control plane de quem consome (Studio)
    /// detectar drift de default sem depender da resposta de uma mutação.
    pub default_version: Option<String>,
    pub health_check: Option<AdminWorkerHealthCheckInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminWorkerHealthCheckInfo {
    pub path: String,
    pub method: String,
    pub mode: String,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminWorkersResponse {
    pub workers: Vec<AdminWorkerInfo>,
}

/// Uma API key como o admin a enxerga: preview, nunca o segredo.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminApiKeyInfo {
    pub id: u64,
    pub name: String,
    pub key_prefix: String,
    pub role: String,
    pub permissions: Vec<String>,
    pub namespaces: Vec<String>,
    pub workers: Vec<String>,
    /// Epoch em segundos, como o resto do vocabulário do store.
    pub created_at: u64,
    pub last_used_at: Option<u64>,
    pub expires_at: Option<u64>,
    pub revoked_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminApiKeysResponse {
    pub keys: Vec<AdminApiKeyInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub permissions: Vec<String>,
    #[serde(default = "star_scope")]
    pub namespaces: Vec<String>,
    #[serde(default = "star_scope")]
    pub workers: Vec<String>,
    pub expires_at: Option<u64>,
    /// `operator` por default. A autorização vem das permissions, com UMA
    /// exceção viva: o health-check manual exige `role` igual a `admin` além de
    /// `workers:read` — ver `follow-ups/api-keys-evolucoes.md`.
    pub role: Option<String>,
}

fn star_scope() -> Vec<String> {
    vec!["*".into()]
}

/// Resposta do create: a ÚNICA vez que `raw_key` existe fora do hash.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminApiKeyCreatedResponse {
    pub key: AdminApiKeyInfo,
    pub raw_key: String,
}
