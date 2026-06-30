use std::sync::Arc;

use edger_core::{KeyValueProvider, QueueProvider, StateValue};
use edger_ext_keyval::SqlKeyValueProvider;
use edger_ext_turso::LocalSqliteProvider;

fn provider() -> SqlKeyValueProvider {
    SqlKeyValueProvider::new(Arc::new(LocalSqliteProvider::in_memory()))
}

#[test]
fn keyval_sets_gets_and_deletes_values() {
    let provider = provider();
    let key = vec!["todos".into(), "1".into()];

    let first = provider
        .set(
            "@acme",
            &key,
            StateValue::Text("Ship state services".into()),
            None,
        )
        .unwrap();
    let second = provider
        .set("@acme", &key, StateValue::Integer(2), None)
        .unwrap();
    let fetched = provider.get("@acme", &key).unwrap().unwrap();

    assert_eq!(first.versionstamp, "1");
    assert_eq!(second.versionstamp, "2");
    assert_eq!(fetched.value, StateValue::Integer(2));
    assert!(provider.delete("@acme", &key).unwrap());
    assert!(provider.get("@acme", &key).unwrap().is_none());
}

#[test]
fn keyval_keeps_namespaces_isolated() {
    let provider = provider();
    let key = vec!["session".into()];
    provider
        .set("@acme", &key, StateValue::Text("acme".into()), None)
        .unwrap();
    provider
        .set("@other", &key, StateValue::Text("other".into()), None)
        .unwrap();

    assert_eq!(
        provider.get("@acme", &key).unwrap().unwrap().value,
        StateValue::Text("acme".into())
    );
    assert_eq!(
        provider.get("@other", &key).unwrap().unwrap().value,
        StateValue::Text("other".into())
    );
}

#[test]
fn queue_enqueues_dequeues_and_acks_messages() {
    let provider = provider();
    let enqueued = provider
        .enqueue("@acme", StateValue::Text("render-report".into()))
        .unwrap();

    let dequeued = provider.dequeue("@acme").unwrap().unwrap();
    assert_eq!(dequeued.id, enqueued.id);
    assert_eq!(dequeued.attempts, 1);
    assert_eq!(dequeued.value, StateValue::Text("render-report".into()));
    assert!(provider.dequeue("@acme").unwrap().is_none());
    assert!(provider.ack("@acme", &dequeued.id).unwrap());
    assert!(!provider.ack("@acme", &dequeued.id).unwrap());
}
