//! edger-ext-keyval — SQL-backed KeyValue and Queue providers.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use edger_core::{
    BindingKind, CoreError, DurableSqlProvider, Extension, ExtensionCapability, ExtensionContext,
    ExtensionDependency, KeyValueEntry, KeyValueProvider, QueueMessage, QueueProvider, StateValue,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// KeyValue and Queue provider implemented over `DurableSqlProvider`.
pub struct SqlKeyValueProvider {
    sql: Arc<dyn DurableSqlProvider>,
}

impl SqlKeyValueProvider {
    pub fn new(sql: Arc<dyn DurableSqlProvider>) -> Self {
        Self { sql }
    }

    fn ensure_kv_schema(&self, namespace: &str) -> Result<(), CoreError> {
        self.sql.execute_batch(
            namespace,
            r#"
            create table if not exists kv_entries (
                key text primary key,
                value text not null,
                version integer not null default 1,
                expires_at integer
            )
            "#,
        )
    }

    fn ensure_queue_schema(&self, namespace: &str) -> Result<(), CoreError> {
        self.sql.execute_batch(
            namespace,
            r#"
            create table if not exists kv_queue (
                id text primary key,
                value text not null,
                status text not null,
                attempts integer not null default 0,
                created_at integer not null
            )
            "#,
        )
    }
}

impl Extension for SqlKeyValueProvider {
    fn capabilities(&self) -> Vec<ExtensionCapability> {
        vec![
            ExtensionCapability::service_provider(BindingKind::KeyValue),
            ExtensionCapability::service_provider(BindingKind::Queue),
        ]
    }

    fn dependencies(&self) -> Vec<ExtensionDependency> {
        vec![ExtensionDependency::capability(
            ExtensionCapability::service_provider(BindingKind::DurableSql),
        )]
    }

    fn name(&self) -> &'static str {
        "keyval"
    }

    fn priority(&self) -> i32 {
        0
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        Ok(())
    }
}

impl KeyValueProvider for SqlKeyValueProvider {
    fn delete(&self, namespace: &str, key: &[String]) -> Result<bool, CoreError> {
        self.ensure_kv_schema(namespace)?;
        let key = encode_key(key)?;
        let affected = self.sql.execute(
            namespace,
            "delete from kv_entries where key = ?",
            &[StateValue::Text(key)],
        )?;
        Ok(affected > 0)
    }

    fn get(&self, namespace: &str, key: &[String]) -> Result<Option<KeyValueEntry>, CoreError> {
        self.ensure_kv_schema(namespace)?;
        let encoded_key = encode_key(key)?;
        let rows = self.sql.query(
            namespace,
            "select value, version, expires_at from kv_entries where key = ?",
            &[StateValue::Text(encoded_key.clone())],
        )?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let expires_at = optional_integer(row.values.get(2))?;
        if expires_at.is_some_and(|expires_at| expires_at <= now_secs()) {
            self.sql.execute(
                namespace,
                "delete from kv_entries where key = ?",
                &[StateValue::Text(encoded_key)],
            )?;
            return Ok(None);
        }
        let value_text = required_text(row.values.first(), "value")?;
        let version = required_integer(row.values.get(1), "version")?;
        Ok(Some(KeyValueEntry {
            key: key.to_vec(),
            value: decode_value(value_text)?,
            versionstamp: version.to_string(),
        }))
    }

    fn set(
        &self,
        namespace: &str,
        key: &[String],
        value: StateValue,
        expires_at: Option<u64>,
    ) -> Result<KeyValueEntry, CoreError> {
        self.ensure_kv_schema(namespace)?;
        let encoded_key = encode_key(key)?;
        let encoded_value = encode_value(&value)?;
        self.sql.execute(
            namespace,
            r#"
            insert into kv_entries (key, value, version, expires_at)
            values (?, ?, 1, ?)
            on conflict(key) do update set
                value = excluded.value,
                version = kv_entries.version + 1,
                expires_at = excluded.expires_at
            "#,
            &[
                StateValue::Text(encoded_key),
                StateValue::Text(encoded_value),
                expires_at
                    .map(|expires_at| StateValue::Integer(expires_at as i64))
                    .unwrap_or(StateValue::Null),
            ],
        )?;
        self.get(namespace, key)?
            .ok_or_else(|| CoreError::new("STORE_ERROR", "key was not persisted"))
    }
}

impl QueueProvider for SqlKeyValueProvider {
    fn ack(&self, namespace: &str, id: &str) -> Result<bool, CoreError> {
        self.ensure_queue_schema(namespace)?;
        let affected = self.sql.execute(
            namespace,
            "delete from kv_queue where id = ?",
            &[StateValue::Text(id.to_string())],
        )?;
        Ok(affected > 0)
    }

    fn dequeue(&self, namespace: &str) -> Result<Option<QueueMessage>, CoreError> {
        self.ensure_queue_schema(namespace)?;
        let rows = self.sql.query(
            namespace,
            r#"
            select id, value, attempts
            from kv_queue
            where status = 'pending'
            order by created_at asc, id asc
            limit 1
            "#,
            &[],
        )?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        let id = required_text(row.values.first(), "id")?.to_string();
        let value = decode_value(required_text(row.values.get(1), "value")?)?;
        let attempts = required_integer(row.values.get(2), "attempts")? as u32 + 1;
        self.sql.execute(
            namespace,
            "update kv_queue set status = 'processing', attempts = ? where id = ?",
            &[
                StateValue::Integer(attempts as i64),
                StateValue::Text(id.clone()),
            ],
        )?;
        Ok(Some(QueueMessage {
            attempts,
            id,
            value,
        }))
    }

    fn enqueue(&self, namespace: &str, value: StateValue) -> Result<QueueMessage, CoreError> {
        self.ensure_queue_schema(namespace)?;
        let id = Uuid::new_v4().to_string();
        self.sql.execute(
            namespace,
            "insert into kv_queue (id, value, status, attempts, created_at) values (?, ?, 'pending', 0, ?)",
            &[
                StateValue::Text(id.clone()),
                StateValue::Text(encode_value(&value)?),
                StateValue::Integer(now_secs() as i64),
            ],
        )?;
        Ok(QueueMessage {
            attempts: 0,
            id,
            value,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", content = "value")]
enum StoredStateValue {
    Bool(bool),
    Bytes(Vec<u8>),
    Float(f64),
    Integer(i64),
    Json(serde_json::Value),
    Null,
    Text(String),
}

fn encode_key(key: &[String]) -> Result<String, CoreError> {
    if key.is_empty() || key.iter().any(|part| part.trim().is_empty()) {
        return Err(CoreError::validation(
            "key",
            "non-empty key parts are required",
        ));
    }
    serde_json::to_string(key).map_err(store_error)
}

fn encode_value(value: &StateValue) -> Result<String, CoreError> {
    let stored = match value {
        StateValue::Bool(value) => StoredStateValue::Bool(*value),
        StateValue::Bytes(value) => StoredStateValue::Bytes(value.clone()),
        StateValue::Float(value) => StoredStateValue::Float(*value),
        StateValue::Integer(value) => StoredStateValue::Integer(*value),
        StateValue::Json(value) => StoredStateValue::Json(value.clone()),
        StateValue::Null => StoredStateValue::Null,
        StateValue::Text(value) => StoredStateValue::Text(value.clone()),
    };
    serde_json::to_string(&stored).map_err(store_error)
}

fn decode_value(value: &str) -> Result<StateValue, CoreError> {
    let stored = serde_json::from_str::<StoredStateValue>(value).map_err(store_error)?;
    Ok(match stored {
        StoredStateValue::Bool(value) => StateValue::Bool(value),
        StoredStateValue::Bytes(value) => StateValue::Bytes(value),
        StoredStateValue::Float(value) => StateValue::Float(value),
        StoredStateValue::Integer(value) => StateValue::Integer(value),
        StoredStateValue::Json(value) => StateValue::Json(value),
        StoredStateValue::Null => StateValue::Null,
        StoredStateValue::Text(value) => StateValue::Text(value),
    })
}

fn required_text<'a>(value: Option<&'a StateValue>, field: &str) -> Result<&'a str, CoreError> {
    match value {
        Some(StateValue::Text(value)) => Ok(value),
        _ => Err(CoreError::new(
            "STORE_ERROR",
            format!("expected text field {field}"),
        )),
    }
}

fn required_integer(value: Option<&StateValue>, field: &str) -> Result<i64, CoreError> {
    match value {
        Some(StateValue::Integer(value)) => Ok(*value),
        _ => Err(CoreError::new(
            "STORE_ERROR",
            format!("expected integer field {field}"),
        )),
    }
}

fn optional_integer(value: Option<&StateValue>) -> Result<Option<u64>, CoreError> {
    match value {
        Some(StateValue::Integer(value)) => u64::try_from(*value)
            .map(Some)
            .map_err(|_| CoreError::new("STORE_ERROR", "expires_at must be positive")),
        Some(StateValue::Null) | None => Ok(None),
        _ => Err(CoreError::new("STORE_ERROR", "expires_at must be integer")),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn store_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::new("STORE_ERROR", error.to_string())
}
