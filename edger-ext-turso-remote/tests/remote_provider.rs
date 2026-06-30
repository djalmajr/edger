use std::path::PathBuf;

use edger_core::{DurableSqlProvider, Extension, StateValue};
use edger_ext_turso_remote::RemoteTursoProvider;

#[test]
fn provider_executes_sql_contract_against_configured_libsql_database() {
    let temp = tempfile::tempdir().unwrap();
    let provider = RemoteTursoProvider::new_local_for_tests(vec![(
        "@acme".to_string(),
        temp.path().join("acme.db"),
    )])
    .unwrap();

    provider
        .execute_batch(
            "@acme",
            "create table todos (
                id integer primary key,
                done integer not null,
                metadata text not null,
                payload blob not null,
                score real not null,
                title text not null
            )",
        )
        .unwrap();
    let affected = provider
        .execute(
            "@acme",
            "insert into todos (done, metadata, payload, score, title) values (?, ?, ?, ?, ?)",
            &[
                StateValue::Bool(true),
                StateValue::Json(serde_json::json!({"source":"test"})),
                StateValue::Bytes(vec![1, 2, 3]),
                StateValue::Float(42.5),
                StateValue::Text("Ship remote provider".into()),
            ],
        )
        .unwrap();

    let rows = provider
        .query(
            "@acme",
            "select done, metadata, payload, score, title from todos where id = ?",
            &[StateValue::Integer(1)],
        )
        .unwrap();

    assert_eq!(affected, 1);
    assert_eq!(
        rows[0].columns,
        vec!["done", "metadata", "payload", "score", "title"]
    );
    assert_eq!(
        rows[0].values,
        vec![
            StateValue::Integer(1),
            StateValue::Text("{\"source\":\"test\"}".into()),
            StateValue::Bytes(vec![1, 2, 3]),
            StateValue::Float(42.5),
            StateValue::Text("Ship remote provider".into()),
        ]
    );
}

#[test]
fn provider_keeps_namespaces_isolated_by_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let provider = RemoteTursoProvider::new_local_for_tests(vec![
        ("@acme".to_string(), temp.path().join("acme.db")),
        ("@other".to_string(), temp.path().join("other.db")),
    ])
    .unwrap();

    for namespace in ["@acme", "@other"] {
        provider
            .execute_batch(namespace, "create table settings (value text not null)")
            .unwrap();
    }
    provider
        .execute(
            "@acme",
            "insert into settings (value) values (?)",
            &[StateValue::Text("acme".into())],
        )
        .unwrap();
    provider
        .execute(
            "@other",
            "insert into settings (value) values (?)",
            &[StateValue::Text("other".into())],
        )
        .unwrap();

    let acme = provider
        .query("@acme", "select value from settings", &[])
        .unwrap()
        .remove(0);
    let other = provider
        .query("@other", "select value from settings", &[])
        .unwrap()
        .remove(0);

    assert_eq!(acme.values, vec![StateValue::Text("acme".into())]);
    assert_eq!(other.values, vec![StateValue::Text("other".into())]);
}

#[test]
fn provider_rejects_unconfigured_namespace_without_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let provider = RemoteTursoProvider::new_local_for_tests(vec![(
        "@acme".to_string(),
        temp.path().join("acme.db"),
    )])
    .unwrap();

    let err = provider
        .query("@other", "select 1", &[])
        .expect_err("unconfigured namespace must fail");

    assert_eq!(err.code, "DURABLE_SQL_CONFIG_ERROR");
}

#[test]
fn provider_reports_extension_capability_without_sensitive_diagnostics() {
    let provider =
        RemoteTursoProvider::new_remote("@acme", "libsql://sensitive.turso.io", "secret-token")
            .unwrap();

    let capabilities = provider
        .capabilities()
        .into_iter()
        .map(|capability| capability.label())
        .collect::<Vec<_>>();
    let diagnostics = provider.diagnostics().unwrap().to_string();

    assert_eq!(provider.name(), "turso-remote");
    assert_eq!(capabilities, vec!["provider:durableSql"]);
    assert!(diagnostics.contains("remote-turso"));
    assert!(!diagnostics.contains("sensitive.turso.io"));
    assert!(!diagnostics.contains("secret-token"));
}

#[test]
fn opt_in_remote_turso_contract_uses_real_configured_target() {
    let Ok(url) = std::env::var("EDGER_TURSO_TEST_URL") else {
        return;
    };
    let Ok(auth_token) = std::env::var("EDGER_TURSO_TEST_AUTH_TOKEN") else {
        return;
    };
    let local_path = std::env::var("EDGER_TURSO_TEST_LOCAL_PATH")
        .ok()
        .map(PathBuf::from);
    let provider = if let Some(local_path) = local_path {
        RemoteTursoProvider::new_remote_replica("@opt-in", url, auth_token, local_path).unwrap()
    } else {
        RemoteTursoProvider::new_remote("@opt-in", url, auth_token).unwrap()
    };

    provider
        .execute_batch(
            "@opt-in",
            "create table if not exists edger_remote_provider_smoke (
                id integer primary key,
                value text not null
            )",
        )
        .unwrap();
    provider
        .execute(
            "@opt-in",
            "insert into edger_remote_provider_smoke (value) values (?)",
            &[StateValue::Text("remote-smoke".into())],
        )
        .unwrap();

    let rows = provider
        .query(
            "@opt-in",
            "select value from edger_remote_provider_smoke order by id desc limit 1",
            &[],
        )
        .unwrap();

    assert_eq!(
        rows[0].values,
        vec![StateValue::Text("remote-smoke".into())]
    );
}
