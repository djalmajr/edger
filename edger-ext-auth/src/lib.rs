//! edger-ext-auth — AuthProvider extension (Epic 06.02).

use std::sync::Arc;

use anyhow::Result;
use edger_core::{
    extract_api_key_from_pairs, root_principal, AdminApiKeyInfo, AdminCreateApiKeyRequest,
    ApiKeyPrincipal, ApiKeyStore, AuthProvider, Extension, ExtensionCapability, ExtensionContext,
    HeaderPairs,
};

pub mod store;

pub use store::SqliteApiKeyStore;

/// Auth extension — API key resolution via SQLite store + optional root key.
pub struct AuthExtension {
    store: Arc<dyn ApiKeyStore>,
    root_api_key: Option<String>,
}

impl AuthExtension {
    pub fn new(store: Arc<dyn ApiKeyStore>, root_api_key: Option<String>) -> Self {
        Self {
            store,
            root_api_key: root_api_key.filter(|s| !s.is_empty()),
        }
    }

    pub fn from_env() -> Result<Self> {
        let store: Arc<dyn ApiKeyStore> = if let Ok(path) = std::env::var("EDGER_AUTH_DB") {
            if !path.is_empty() {
                Arc::new(SqliteApiKeyStore::open(path)?)
            } else {
                Arc::new(SqliteApiKeyStore::in_memory()?)
            }
        } else {
            Arc::new(SqliteApiKeyStore::in_memory()?)
        };
        let root_api_key = std::env::var("ROOT_API_KEY").ok();
        Ok(Self::new(store, root_api_key))
    }

    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

impl Extension for AuthExtension {
    fn capabilities(&self) -> Vec<ExtensionCapability> {
        vec![
            ExtensionCapability::auth_provider(),
            ExtensionCapability::ApiKeys,
        ]
    }

    fn name(&self) -> &'static str {
        "auth"
    }

    fn priority(&self) -> i32 {
        -100
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        Ok(())
    }
}

impl AuthProvider for AuthExtension {
    fn authenticate(&self, headers: &HeaderPairs) -> Result<Option<ApiKeyPrincipal>> {
        let Some(raw_key) = extract_api_key_from_pairs(headers) else {
            return Ok(None);
        };
        if self
            .root_api_key
            .as_ref()
            .is_some_and(|root| root == &raw_key)
        {
            return Ok(Some(root_principal()));
        }
        self.store.lookup_by_key(&raw_key).map_err(Into::into)
    }

    fn list_api_keys(&self) -> Result<Vec<AdminApiKeyInfo>> {
        self.store.list_keys().map_err(Into::into)
    }

    fn create_api_key(
        &self,
        raw_key: &str,
        request: &AdminCreateApiKeyRequest,
    ) -> Result<AdminApiKeyInfo> {
        let role = if request.role.trim().is_empty() {
            "viewer"
        } else {
            request.role.trim()
        };
        let id = self.store.insert_key(
            raw_key,
            request.name.trim(),
            role,
            &request.permissions,
            &request.namespaces,
            request.expires_at,
        )?;
        self.store
            .list_keys()?
            .into_iter()
            .find(|key| key.id == id)
            .ok_or_else(|| anyhow::anyhow!("created API key metadata is missing"))
    }

    fn revoke_api_key(&self, id: u64) -> Result<bool> {
        self.store.revoke_key(id).map_err(Into::into)
    }
}
