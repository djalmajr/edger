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
fn extract_api_key_from_pairs_bearer() {
    let headers = vec![("Authorization".into(), "Bearer abc".into())];
    assert_eq!(extract_api_key_from_pairs(&headers).as_deref(), Some("abc"));
}
