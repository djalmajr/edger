//! Admin API vocabulary. Pure response/request shapes only.

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminWorkerInfo {
    pub kind: ExecutionKind,
    pub name: String,
    pub namespace: Option<String>,
    pub plugin_base: Option<String>,
    pub source: String,
    pub status: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminWorkersResponse {
    pub workers: Vec<AdminWorkerInfo>,
}
