use edger_core::{DurableSqlProvider, StateValue};
use edger_ext_turso::{LocalSqliteProvider, LocalTursoProvider};

#[test]
fn local_provider_executes_sql_per_namespace() {
    let provider = LocalSqliteProvider::in_memory();
    provider
        .execute_batch(
            "@acme",
            "create table todos (id integer primary key, title text not null)",
        )
        .unwrap();
    let affected = provider
        .execute(
            "@acme",
            "insert into todos (title) values (?)",
            &[StateValue::Text("Ship state bindings".into())],
        )
        .unwrap();

    let rows = provider
        .query(
            "@acme",
            "select title from todos where id = ?",
            &[StateValue::Integer(1)],
        )
        .unwrap();

    assert_eq!(affected, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].columns, vec!["title"]);
    assert_eq!(
        rows[0].values,
        vec![StateValue::Text("Ship state bindings".into())]
    );
}

#[test]
fn local_provider_keeps_namespaces_isolated() {
    let provider = LocalSqliteProvider::in_memory();
    for namespace in ["@acme", "@other"] {
        provider
            .execute_batch(namespace, "create table kv (value text not null)")
            .unwrap();
    }
    provider
        .execute(
            "@acme",
            "insert into kv (value) values (?)",
            &[StateValue::Text("acme".into())],
        )
        .unwrap();
    provider
        .execute(
            "@other",
            "insert into kv (value) values (?)",
            &[StateValue::Text("other".into())],
        )
        .unwrap();

    let acme = provider
        .query("@acme", "select value from kv", &[])
        .unwrap()
        .remove(0);
    let other = provider
        .query("@other", "select value from kv", &[])
        .unwrap()
        .remove(0);

    assert_eq!(acme.values, vec![StateValue::Text("acme".into())]);
    assert_eq!(other.values, vec![StateValue::Text("other".into())]);
}

#[test]
fn local_provider_persists_file_backed_namespace() {
    let temp = tempfile::tempdir().unwrap();
    {
        let provider = LocalSqliteProvider::open_dir(temp.path()).unwrap();
        provider
            .execute_batch(
                "@acme",
                "create table settings (key text primary key, value text not null)",
            )
            .unwrap();
        provider
            .execute(
                "@acme",
                "insert into settings (key, value) values (?, ?)",
                &[
                    StateValue::Text("theme".into()),
                    StateValue::Text("dark".into()),
                ],
            )
            .unwrap();
    }

    let provider = LocalSqliteProvider::open_dir(temp.path()).unwrap();
    let rows = provider
        .query(
            "@acme",
            "select value from settings where key = ?",
            &[StateValue::Text("theme".into())],
        )
        .unwrap();

    assert_eq!(rows[0].values, vec![StateValue::Text("dark".into())]);
}

#[test]
fn legacy_local_turso_alias_keeps_sql_provider_compatibility() {
    let provider = LocalTursoProvider::in_memory();
    provider
        .execute_batch("@acme", "create table aliases (value text not null)")
        .unwrap();
    provider
        .execute(
            "@acme",
            "insert into aliases (value) values (?)",
            &[StateValue::Text("compatible".into())],
        )
        .unwrap();

    let rows = provider
        .query("@acme", "select value from aliases", &[])
        .unwrap();

    assert_eq!(rows[0].values, vec![StateValue::Text("compatible".into())]);
}
