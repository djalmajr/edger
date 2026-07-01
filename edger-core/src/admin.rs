//! Admin API vocabulary. Pure response/request shapes only.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ApiKeyPrincipal, ExecutionKind};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminApiKeyInfo {
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub id: u64,
    pub is_root: bool,
    pub key_prefix: String,
    pub name: String,
    pub namespaces: Vec<String>,
    pub permissions: Vec<String>,
    pub role: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminApiKeysResponse {
    pub keys: Vec<AdminApiKeyInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminCreateApiKeyRequest {
    pub name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub namespaces: Vec<String>,
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminCreateApiKeyResponse {
    pub key: AdminApiKeyInfo,
    pub raw_key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminRevokeApiKeyResponse {
    pub id: u64,
    pub revoked: bool,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminErrorResponse {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminExtensionManifest {
    pub config: AdminExtensionManifestConfig,
    pub hooks: Vec<String>,
    pub menus: Vec<AdminExtensionManifestMenu>,
    pub provides: Vec<String>,
    pub requirements: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminExtensionManifestConfig {
    pub keys: Vec<String>,
    pub redacted: bool,
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminExtensionManifestMenu {
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminExtensionInfo {
    pub capabilities: Vec<String>,
    pub config_source: String,
    pub dependencies: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Value>,
    pub id: String,
    pub kind: String,
    pub manifest: AdminExtensionManifest,
    pub name: String,
    pub priority: i32,
    pub status: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminExtensionsResponse {
    pub extensions: Vec<AdminExtensionInfo>,
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
    pub public_routes: Vec<String>,
    pub source: String,
    pub status: String,
    pub version: String,
    pub visibility: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdminWorkersResponse {
    pub workers: Vec<AdminWorkerInfo>,
}
