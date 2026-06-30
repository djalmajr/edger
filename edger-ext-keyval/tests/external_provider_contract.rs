use std::sync::Arc;

use edger_core::{KeyValueProvider, QueueProvider, StateValue};
use edger_ext_keyval::SqlKeyValueProvider;
use edger_ext_turso_remote::RemoteTursoProvider;

#[test]
fn keyval_and_queue_preserve_contract_with_external_durable_sql_provider() {
    let temp = tempfile::tempdir().unwrap();
    let provider = SqlKeyValueProvider::new(Arc::new(
        RemoteTursoProvider::new_local_for_tests(vec![(
            "@acme".to_string(),
            temp.path().join("acme-state.db"),
        )])
        .unwrap(),
    ));
    let key = vec!["todos".into(), "42".into()];

    let entry = provider
        .set(
            "@acme",
            &key,
            StateValue::Text("prove external provider".into()),
            None,
        )
        .unwrap();
    let fetched = provider.get("@acme", &key).unwrap().unwrap();
    let enqueued = provider
        .enqueue(
            "@acme",
            StateValue::Json(serde_json::json!({ "job": "sync" })),
        )
        .unwrap();
    let dequeued = provider.dequeue("@acme").unwrap().unwrap();

    assert_eq!(entry.versionstamp, "1");
    assert_eq!(fetched.key, key);
    assert_eq!(
        fetched.value,
        StateValue::Text("prove external provider".into())
    );
    assert_eq!(dequeued.id, enqueued.id);
    assert_eq!(dequeued.attempts, 1);
    assert_eq!(
        dequeued.value,
        StateValue::Json(serde_json::json!({ "job": "sync" }))
    );
    assert!(provider.ack("@acme", &dequeued.id).unwrap());
    assert!(provider.delete("@acme", &key).unwrap());
}
