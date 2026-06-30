//! AuthProvider tests (story 06.02).

use edger_core::{extract_api_key_from_pairs, ApiKeyStore, AuthProvider};
use edger_ext_auth::{AuthExtension, SqliteApiKeyStore};
use std::sync::Arc;

#[test]
fn authenticate_without_header_returns_none() {
    let ext = AuthExtension::new(Arc::new(SqliteApiKeyStore::in_memory().unwrap()), None);
    assert!(ext.authenticate(&[]).unwrap().is_none());
}

#[test]
fn root_key_returns_synthetic_principal() {
    let ext = AuthExtension::new(
        Arc::new(SqliteApiKeyStore::in_memory().unwrap()),
        Some("root-secret".into()),
    );
    let headers = vec![("authorization".into(), "Bearer root-secret".into())];
    let principal = ext.authenticate(&headers).unwrap().unwrap();
    assert!(principal.is_root);
}

#[test]
fn store_key_resolves_namespaces() {
    let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
    store
        .insert_key("btk_acme", "acme", "editor", &[], &["@acme".into()], None)
        .unwrap();
    let ext = AuthExtension::new(store, None);
    let headers = vec![("x-api-key".into(), "btk_acme".into())];
    let principal = ext.authenticate(&headers).unwrap().unwrap();
    assert_eq!(principal.namespaces, vec!["@acme"]);
}

#[test]
fn file_backed_store_bootstraps_auth_without_durable_sql_provider() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("auth.db");
    let raw_key = "btk_bootstrap_file";

    {
        let store = Arc::new(SqliteApiKeyStore::open(&db_path).unwrap());
        let id = store
            .insert_key(
                raw_key,
                "bootstrap",
                "admin",
                &["workers:install".into()],
                &["@acme".into()],
                None,
            )
            .unwrap();
        let ext = AuthExtension::new(store, Some("root-secret".into()));

        let root = ext
            .authenticate(&[("authorization".into(), "Bearer root-secret".into())])
            .unwrap()
            .unwrap();
        assert!(root.is_root);

        let principal = ext
            .authenticate(&[("x-api-key".into(), raw_key.into())])
            .unwrap()
            .unwrap();
        assert_eq!(principal.id, id);
        assert_eq!(principal.role, "admin");
        assert_eq!(principal.permissions, vec!["workers:install"]);
        assert_eq!(principal.namespaces, vec!["@acme"]);
        assert!(!principal.is_root);
    }

    let reopened = Arc::new(SqliteApiKeyStore::open(&db_path).unwrap());
    let ext = AuthExtension::new(reopened, None);
    let principal = ext
        .authenticate(&[("authorization".into(), format!("Bearer {raw_key}"))])
        .unwrap()
        .expect("file-backed API key survives reopen");

    assert_eq!(principal.name, "bootstrap");
    assert_eq!(principal.permissions, vec!["workers:install"]);
    assert_eq!(principal.namespaces, vec!["@acme"]);
    assert!(!principal.is_root);
}

#[test]
fn revoked_store_key_no_longer_authenticates() {
    let store = Arc::new(SqliteApiKeyStore::in_memory().unwrap());
    let id = store
        .insert_key(
            "btk_revoked",
            "revoked",
            "viewer",
            &["workers:read".into()],
            &["@acme".into()],
            None,
        )
        .unwrap();
    assert!(store.revoke_key(id).unwrap());
    assert!(!store.revoke_key(id).unwrap());

    let ext = AuthExtension::new(store, None);
    let headers = vec![("authorization".into(), "Bearer btk_revoked".into())];
    assert!(ext.authenticate(&headers).unwrap().is_none());
}

#[test]
fn extract_api_key_from_pairs_bearer() {
    let headers = vec![("Authorization".into(), "Bearer abc".into())];
    assert_eq!(extract_api_key_from_pairs(&headers).as_deref(), Some("abc"));
}
