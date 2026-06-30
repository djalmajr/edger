//! Pure service binding and state provider contracts.

use serde::{Deserialize, Serialize};

use crate::extension::Extension;
use crate::CoreError;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BindingDescriptor {
    pub kind: BindingKind,
    pub name: String,
    pub namespace: String,
    pub permissions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BindingManifest {
    pub kind: BindingKind,
    pub name: String,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum BindingKind {
    DurableSql,
    KeyValue,
    Queue,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BindingSet {
    pub bindings: Vec<BindingDescriptor>,
    pub worker: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QueueMessage {
    pub attempts: u32,
    pub id: String,
    pub value: StateValue,
}

pub type StateKey = Vec<String>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyValueEntry {
    pub key: StateKey,
    pub value: StateValue,
    pub versionstamp: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum StateValue {
    Bool(bool),
    Bytes(Vec<u8>),
    Float(f64),
    Integer(i64),
    Json(serde_json::Value),
    Null,
    Text(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SqlRow {
    pub columns: Vec<String>,
    pub values: Vec<StateValue>,
}

pub trait DurableSqlProvider: Extension {
    fn execute(&self, namespace: &str, sql: &str, params: &[StateValue]) -> Result<u64, CoreError>;

    fn execute_batch(&self, namespace: &str, sql: &str) -> Result<(), CoreError>;

    fn query(
        &self,
        namespace: &str,
        sql: &str,
        params: &[StateValue],
    ) -> Result<Vec<SqlRow>, CoreError>;
}

pub trait KeyValueProvider: Extension {
    fn delete(&self, namespace: &str, key: &[String]) -> Result<bool, CoreError>;
    fn get(&self, namespace: &str, key: &[String]) -> Result<Option<KeyValueEntry>, CoreError>;
    fn set(
        &self,
        namespace: &str,
        key: &[String],
        value: StateValue,
        expires_at: Option<u64>,
    ) -> Result<KeyValueEntry, CoreError>;
}

pub trait QueueProvider: Extension {
    fn ack(&self, namespace: &str, id: &str) -> Result<bool, CoreError>;
    fn dequeue(&self, namespace: &str) -> Result<Option<QueueMessage>, CoreError>;
    fn enqueue(&self, namespace: &str, value: StateValue) -> Result<QueueMessage, CoreError>;
}

pub fn binding_descriptor(
    binding: &BindingManifest,
    default_namespace: &str,
) -> Result<BindingDescriptor, CoreError> {
    if binding.name.trim().is_empty() {
        return Err(CoreError::validation("binding.name", "name is required"));
    }
    let namespace = binding
        .namespace
        .as_deref()
        .filter(|namespace| !namespace.trim().is_empty())
        .unwrap_or(default_namespace)
        .to_string();
    Ok(BindingDescriptor {
        kind: binding.kind.clone(),
        name: binding.name.clone(),
        namespace,
        permissions: binding.permissions.clone(),
    })
}

pub fn default_binding_namespace(worker_name: &str, worker_namespace: Option<&str>) -> String {
    worker_namespace
        .filter(|namespace| !namespace.trim().is_empty())
        .unwrap_or(worker_name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_uses_default_namespace_when_missing() {
        let manifest = BindingManifest {
            kind: BindingKind::KeyValue,
            name: "kv".into(),
            namespace: None,
            permissions: vec!["kv:read".into()],
        };
        let descriptor = binding_descriptor(&manifest, "@acme").unwrap();
        assert_eq!(descriptor.namespace, "@acme");
        assert_eq!(descriptor.permissions, vec!["kv:read"]);
    }

    #[test]
    fn descriptor_rejects_empty_binding_name() {
        let manifest = BindingManifest {
            kind: BindingKind::Queue,
            name: " ".into(),
            namespace: None,
            permissions: vec![],
        };
        let err = binding_descriptor(&manifest, "worker").unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
    }
}
