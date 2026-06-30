//! edger-ext-turso — Durable SQL provider backed by local SQLite.
//!
//! This crate keeps the historical `edger-ext-turso` package name, but the
//! implementation in this crate is local/single-node SQLite. Remote Turso
//! transport belongs behind the same `DurableSqlProvider` contract as a
//! separate provider.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use edger_core::{
    BindingKind, CoreError, DurableSqlProvider, Extension, ExtensionCapability, ExtensionContext,
    SqlRow, StateValue,
};
use rusqlite::types::{Value, ValueRef};
use rusqlite::{params_from_iter, Connection};

/// Durable SQL extension/provider using one local SQLite database per namespace.
pub struct LocalSqliteProvider {
    connections: Mutex<HashMap<String, Connection>>,
    root: Option<PathBuf>,
}

/// Compatibility alias for the historical local provider name.
pub type LocalTursoProvider = LocalSqliteProvider;

impl LocalSqliteProvider {
    pub fn in_memory() -> Self {
        Self {
            connections: Mutex::new(HashMap::new()),
            root: None,
        }
    }

    pub fn open_dir(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let root = path.as_ref().to_path_buf();
        std::fs::create_dir_all(&root).map_err(store_error)?;
        Ok(Self {
            connections: Mutex::new(HashMap::new()),
            root: Some(root),
        })
    }

    fn with_connection<T>(
        &self,
        namespace: &str,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, CoreError> {
        if namespace.trim().is_empty() {
            return Err(CoreError::validation("namespace", "namespace is required"));
        }
        let key = sanitize_namespace(namespace);
        let mut connections = self
            .connections
            .lock()
            .map_err(|_| CoreError::new("STORE_ERROR", "connection lock poisoned"))?;
        if !connections.contains_key(&key) {
            let connection = if let Some(root) = &self.root {
                Connection::open(root.join(format!("{key}.db"))).map_err(store_error)?
            } else {
                Connection::open_in_memory().map_err(store_error)?
            };
            connections.insert(key.clone(), connection);
        }
        let connection = connections
            .get_mut(&key)
            .ok_or_else(|| CoreError::new("STORE_ERROR", "connection not available"))?;
        f(connection).map_err(store_error)
    }
}

impl Default for LocalSqliteProvider {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl Extension for LocalSqliteProvider {
    fn capabilities(&self) -> Vec<ExtensionCapability> {
        vec![ExtensionCapability::service_provider(
            BindingKind::DurableSql,
        )]
    }

    fn name(&self) -> &'static str {
        "turso"
    }

    fn priority(&self) -> i32 {
        0
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        Ok(())
    }
}

impl DurableSqlProvider for LocalSqliteProvider {
    fn execute(&self, namespace: &str, sql: &str, params: &[StateValue]) -> Result<u64, CoreError> {
        let params = params.iter().map(sqlite_value).collect::<Vec<_>>();
        self.with_connection(namespace, |connection| {
            connection
                .execute(sql, params_from_iter(params.iter()))
                .map(|affected| affected as u64)
        })
    }

    fn execute_batch(&self, namespace: &str, sql: &str) -> Result<(), CoreError> {
        self.with_connection(namespace, |connection| connection.execute_batch(sql))
    }

    fn query(
        &self,
        namespace: &str,
        sql: &str,
        params: &[StateValue],
    ) -> Result<Vec<SqlRow>, CoreError> {
        let params = params.iter().map(sqlite_value).collect::<Vec<_>>();
        self.with_connection(namespace, |connection| {
            let mut statement = connection.prepare(sql)?;
            let columns = statement
                .column_names()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            let mut rows = statement.query(params_from_iter(params.iter()))?;
            let mut result = Vec::new();
            while let Some(row) = rows.next()? {
                let mut values = Vec::with_capacity(columns.len());
                for index in 0..columns.len() {
                    values.push(state_value(row.get_ref(index)?));
                }
                result.push(SqlRow {
                    columns: columns.clone(),
                    values,
                });
            }
            Ok(result)
        })
    }
}

fn sanitize_namespace(namespace: &str) -> String {
    let safe = namespace
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        "default".into()
    } else {
        safe
    }
}

fn sqlite_value(value: &StateValue) -> Value {
    match value {
        StateValue::Bool(value) => Value::Integer(i64::from(*value)),
        StateValue::Bytes(value) => Value::Blob(value.clone()),
        StateValue::Float(value) => Value::Real(*value),
        StateValue::Integer(value) => Value::Integer(*value),
        StateValue::Json(value) => Value::Text(value.to_string()),
        StateValue::Null => Value::Null,
        StateValue::Text(value) => Value::Text(value.clone()),
    }
}

fn state_value(value: ValueRef<'_>) -> StateValue {
    match value {
        ValueRef::Null => StateValue::Null,
        ValueRef::Integer(value) => StateValue::Integer(value),
        ValueRef::Real(value) => StateValue::Float(value),
        ValueRef::Text(value) => StateValue::Text(String::from_utf8_lossy(value).into_owned()),
        ValueRef::Blob(value) => StateValue::Bytes(value.to_vec()),
    }
}

fn store_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::new("STORE_ERROR", error.to_string())
}
