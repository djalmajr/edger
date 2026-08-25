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
