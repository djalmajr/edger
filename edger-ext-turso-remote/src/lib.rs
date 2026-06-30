//! Remote/sync Turso provider implemented over libSQL.
//!
//! This crate is intentionally separate from `edger-core` and
//! `edger-orchestrator`. It adapts libSQL/Turso transport details to the
//! stable `DurableSqlProvider` contract.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use edger_core::{
    BindingKind, CoreError, DurableSqlProvider, Extension, ExtensionCapability, ExtensionContext,
    SqlRow, StateValue,
};
use libsql::{params_from_iter, Builder, Database, Value};
use serde_json::json;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

const CONFIG_ERROR: &str = "DURABLE_SQL_CONFIG_ERROR";
const REMOTE_ERROR: &str = "DURABLE_SQL_REMOTE_ERROR";
const SYNC_ERROR: &str = "DURABLE_SQL_SYNC_ERROR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteTursoMode {
    #[doc(hidden)]
    LocalForTests {
        local_path: PathBuf,
    },
    Remote,
    RemoteReplica {
        local_path: PathBuf,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTursoNamespaceConfig {
    pub auth_token: String,
    pub mode: RemoteTursoMode,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteTursoConfig {
    namespaces: HashMap<String, RemoteTursoNamespaceConfig>,
}

/// Durable SQL provider backed by remote libSQL/Turso.
pub struct RemoteTursoProvider {
    config: RemoteTursoConfig,
    databases: Mutex<HashMap<String, Arc<Database>>>,
    runtime: Runtime,
}

impl RemoteTursoConfig {
    pub fn new() -> Self {
        Self {
            namespaces: HashMap::new(),
        }
    }

    pub fn insert_namespace(
        mut self,
        namespace: impl Into<String>,
        config: RemoteTursoNamespaceConfig,
    ) -> Result<Self, CoreError> {
        let namespace = checked_namespace(&namespace.into())?;
        validate_namespace_config(&config)?;
        self.namespaces.insert(namespace, config);
        Ok(self)
    }

    pub fn namespace_count(&self) -> usize {
        self.namespaces.len()
    }
}

impl RemoteTursoNamespaceConfig {
    pub fn remote(url: impl Into<String>, auth_token: impl Into<String>) -> Self {
        Self {
            auth_token: auth_token.into(),
            mode: RemoteTursoMode::Remote,
            url: url.into(),
        }
    }

    pub fn remote_replica(
        url: impl Into<String>,
        auth_token: impl Into<String>,
        local_path: impl AsRef<Path>,
    ) -> Self {
        Self {
            auth_token: auth_token.into(),
            mode: RemoteTursoMode::RemoteReplica {
                local_path: local_path.as_ref().to_path_buf(),
            },
            url: url.into(),
        }
    }
}

impl RemoteTursoProvider {
    pub fn from_config(config: RemoteTursoConfig) -> Result<Self, CoreError> {
        if config.namespace_count() == 0 {
            return Err(CoreError::new(
                CONFIG_ERROR,
                "at least one namespace must be configured",
            ));
        }
        Ok(Self {
            config,
            databases: Mutex::new(HashMap::new()),
            runtime: RuntimeBuilder::new_multi_thread()
                .enable_all()
                .thread_name("edger-turso-remote")
                .build()
                .map_err(config_error)?,
        })
    }

    pub fn from_env() -> Result<Self, CoreError> {
        let namespace = std::env::var("EDGER_TURSO_NAMESPACE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "default".into());
        let url = required_env("EDGER_TURSO_URL")?;
        let auth_token = required_env("EDGER_TURSO_AUTH_TOKEN")?;
        let mode = std::env::var("EDGER_TURSO_MODE").unwrap_or_else(|_| "remote".into());
        let namespace_config = match mode.as_str() {
            "remote" => RemoteTursoNamespaceConfig::remote(url, auth_token),
            "sync" | "remote-replica" | "remote_replica" => {
                let local_path = required_env("EDGER_TURSO_LOCAL_PATH")?;
                RemoteTursoNamespaceConfig::remote_replica(url, auth_token, local_path)
            }
            _ => {
                return Err(CoreError::validation(
                    "EDGER_TURSO_MODE",
                    "expected remote or sync",
                ))
            }
        };
        Self::from_config(RemoteTursoConfig::new().insert_namespace(namespace, namespace_config)?)
    }

    pub fn new_remote(
        namespace: impl Into<String>,
        url: impl Into<String>,
        auth_token: impl Into<String>,
    ) -> Result<Self, CoreError> {
        Self::from_config(RemoteTursoConfig::new().insert_namespace(
            namespace,
            RemoteTursoNamespaceConfig::remote(url, auth_token),
        )?)
    }

    pub fn new_remote_replica(
        namespace: impl Into<String>,
        url: impl Into<String>,
        auth_token: impl Into<String>,
        local_path: impl AsRef<Path>,
    ) -> Result<Self, CoreError> {
        Self::from_config(RemoteTursoConfig::new().insert_namespace(
            namespace,
            RemoteTursoNamespaceConfig::remote_replica(url, auth_token, local_path),
        )?)
    }

    pub fn mode_for_namespace(&self, namespace: &str) -> Result<&'static str, CoreError> {
        let config = self.namespace_config(namespace)?;
        Ok(match &config.mode {
            RemoteTursoMode::LocalForTests { .. } => "local-test",
            RemoteTursoMode::Remote => "remote",
            RemoteTursoMode::RemoteReplica { .. } => "sync",
        })
    }

    #[doc(hidden)]
    pub fn new_local_for_tests(
        namespace_paths: impl IntoIterator<Item = (String, PathBuf)>,
    ) -> Result<Self, CoreError> {
        let mut config = RemoteTursoConfig::new();
        for (namespace, path) in namespace_paths {
            config = config.insert_namespace(
                namespace,
                RemoteTursoNamespaceConfig {
                    auth_token: String::new(),
                    mode: RemoteTursoMode::LocalForTests { local_path: path },
                    url: "file://local-test".into(),
                },
            )?;
        }
        let provider = Self::from_config(config)?;
        for (namespace, namespace_config) in &provider.config.namespaces {
            if let RemoteTursoMode::LocalForTests { local_path } = &namespace_config.mode {
                let database = provider.runtime.block_on(async {
                    Builder::new_local(local_path)
                        .build()
                        .await
                        .map_err(remote_error)
                })?;
                provider
                    .databases
                    .lock()
                    .map_err(|_| CoreError::new(REMOTE_ERROR, "database lock poisoned"))?
                    .insert(namespace.clone(), Arc::new(database));
            }
        }
        Ok(provider)
    }

    fn database(&self, namespace: &str) -> Result<Arc<Database>, CoreError> {
        let namespace = checked_namespace(namespace)?;
        if let Some(database) = self
            .databases
            .lock()
            .map_err(|_| CoreError::new(REMOTE_ERROR, "database lock poisoned"))?
            .get(&namespace)
            .cloned()
        {
            return Ok(database);
        }

        let namespace_config = self.namespace_config(&namespace)?.clone();
        let database = self.runtime.block_on(async {
            build_database(&namespace, &namespace_config)
                .await
                .map(Arc::new)
        })?;
        self.databases
            .lock()
            .map_err(|_| CoreError::new(REMOTE_ERROR, "database lock poisoned"))?
            .insert(namespace, database.clone());
        Ok(database)
    }

    fn namespace_config(&self, namespace: &str) -> Result<&RemoteTursoNamespaceConfig, CoreError> {
        let namespace = checked_namespace(namespace)?;
        self.config.namespaces.get(&namespace).ok_or_else(|| {
            CoreError::new(
                CONFIG_ERROR,
                format!("namespace `{namespace}` is not configured for remote Turso"),
            )
        })
    }

    fn sync_before_query(
        &self,
        database: &Database,
        config: &RemoteTursoNamespaceConfig,
    ) -> Result<(), CoreError> {
        if matches!(config.mode, RemoteTursoMode::RemoteReplica { .. }) {
            self.runtime
                .block_on(async { database.sync().await.map(|_| ()).map_err(sync_error) })?;
        }
        Ok(())
    }

    fn sync_after_write(
        &self,
        database: &Database,
        config: &RemoteTursoNamespaceConfig,
    ) -> Result<(), CoreError> {
        if matches!(config.mode, RemoteTursoMode::RemoteReplica { .. }) {
            self.runtime
                .block_on(async { database.sync().await.map(|_| ()).map_err(sync_error) })?;
        }
        Ok(())
    }
}

impl Extension for RemoteTursoProvider {
    fn capabilities(&self) -> Vec<ExtensionCapability> {
        vec![ExtensionCapability::service_provider(
            BindingKind::DurableSql,
        )]
    }

    fn diagnostics(&self) -> Option<serde_json::Value> {
        let namespaces = self
            .config
            .namespaces
            .iter()
            .map(|(namespace, config)| {
                json!({
                    "mode": match &config.mode {
                        RemoteTursoMode::LocalForTests { .. } => "local-test",
                        RemoteTursoMode::Remote => "remote",
                        RemoteTursoMode::RemoteReplica { .. } => "sync",
                    },
                    "namespace": namespace,
                    "url": redact_url(&config.url),
                })
            })
            .collect::<Vec<_>>();
        Some(json!({
            "configuredNamespaces": namespaces.len(),
            "provider": "remote-turso",
            "namespaces": namespaces,
        }))
    }

    fn name(&self) -> &'static str {
        "turso-remote"
    }

    fn priority(&self) -> i32 {
        0
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        Ok(())
    }
}

impl DurableSqlProvider for RemoteTursoProvider {
    fn execute(&self, namespace: &str, sql: &str, params: &[StateValue]) -> Result<u64, CoreError> {
        let namespace_config = self.namespace_config(namespace)?.clone();
        let database = self.database(namespace)?;
        let values = params.iter().map(libsql_value).collect::<Vec<_>>();
        let affected = self.runtime.block_on(async {
            let connection = database.connect().map_err(remote_error)?;
            connection
                .execute(sql, params_from_iter(values))
                .await
                .map_err(remote_error)
        })?;
        self.sync_after_write(&database, &namespace_config)?;
        Ok(affected)
    }

    fn execute_batch(&self, namespace: &str, sql: &str) -> Result<(), CoreError> {
        let namespace_config = self.namespace_config(namespace)?.clone();
        let database = self.database(namespace)?;
        self.runtime.block_on(async {
            let connection = database.connect().map_err(remote_error)?;
            connection
                .execute_batch(sql)
                .await
                .map(|_| ())
                .map_err(remote_error)
        })?;
        self.sync_after_write(&database, &namespace_config)
    }

    fn query(
        &self,
        namespace: &str,
        sql: &str,
        params: &[StateValue],
    ) -> Result<Vec<SqlRow>, CoreError> {
        let namespace_config = self.namespace_config(namespace)?.clone();
        let database = self.database(namespace)?;
        self.sync_before_query(&database, &namespace_config)?;
        let values = params.iter().map(libsql_value).collect::<Vec<_>>();
        self.runtime.block_on(async {
            let connection = database.connect().map_err(remote_error)?;
            let mut rows = connection
                .query(sql, params_from_iter(values))
                .await
                .map_err(remote_error)?;
            let column_count = rows.column_count();
            let columns = (0..column_count)
                .map(|index| rows.column_name(index).unwrap_or("").to_string())
                .collect::<Vec<_>>();
            let mut result = Vec::new();
            while let Some(row) = rows.next().await.map_err(remote_error)? {
                let values = (0..column_count)
                    .map(|index| row.get_value(index).map(state_value).map_err(remote_error))
                    .collect::<Result<Vec<_>, _>>()?;
                result.push(SqlRow {
                    columns: columns.clone(),
                    values,
                });
            }
            Ok(result)
        })
    }
}

async fn build_database(
    namespace: &str,
    config: &RemoteTursoNamespaceConfig,
) -> Result<Database, CoreError> {
    match &config.mode {
        RemoteTursoMode::LocalForTests { local_path } => Builder::new_local(local_path)
            .build()
            .await
            .map_err(remote_error),
        RemoteTursoMode::Remote => {
            Builder::new_remote(config.url.clone(), config.auth_token.clone())
                .namespace(namespace)
                .build()
                .await
                .map_err(remote_error)
        }
        RemoteTursoMode::RemoteReplica { local_path } => {
            Builder::new_remote_replica(local_path, config.url.clone(), config.auth_token.clone())
                .namespace(namespace)
                .build()
                .await
                .map_err(remote_error)
        }
    }
}

fn checked_namespace(namespace: &str) -> Result<String, CoreError> {
    let namespace = namespace.trim();
    if namespace.is_empty() {
        return Err(CoreError::validation("namespace", "namespace is required"));
    }
    Ok(namespace.to_string())
}

fn config_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::new(CONFIG_ERROR, error.to_string())
}

fn required_env(name: &str) -> Result<String, CoreError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| CoreError::validation(name, "required for remote Turso provider"))
}

fn validate_namespace_config(config: &RemoteTursoNamespaceConfig) -> Result<(), CoreError> {
    if config.url.trim().is_empty() {
        return Err(CoreError::validation("url", "url is required"));
    }
    if config.auth_token.trim().is_empty() && !config.url.starts_with("file://") {
        return Err(CoreError::validation(
            "authToken",
            "auth token is required for remote Turso",
        ));
    }
    if let RemoteTursoMode::RemoteReplica { local_path }
    | RemoteTursoMode::LocalForTests { local_path } = &config.mode
    {
        if local_path.as_os_str().is_empty() {
            return Err(CoreError::validation(
                "localPath",
                "local path is required for sync mode",
            ));
        }
    }
    Ok(())
}

fn libsql_value(value: &StateValue) -> Value {
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

fn state_value(value: Value) -> StateValue {
    match value {
        Value::Null => StateValue::Null,
        Value::Integer(value) => StateValue::Integer(value),
        Value::Real(value) => StateValue::Float(value),
        Value::Text(value) => StateValue::Text(value),
        Value::Blob(value) => StateValue::Bytes(value),
    }
}

fn remote_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::new(REMOTE_ERROR, redact_error(&error.to_string()))
}

fn sync_error(error: impl std::fmt::Display) -> CoreError {
    CoreError::new(SYNC_ERROR, redact_error(&error.to_string()))
}

fn redact_error(message: &str) -> String {
    let mut redact_next = false;
    let mut parts = Vec::new();
    for part in message.split_whitespace() {
        if redact_next {
            parts.push("[redacted]");
            redact_next = false;
            continue;
        }
        if part.starts_with("libsql://")
            || part.starts_with("http://")
            || part.starts_with("https://")
        {
            parts.push("[redacted]");
            continue;
        }
        if part.trim_end_matches(':').eq_ignore_ascii_case("bearer") {
            parts.push("[redacted]");
            redact_next = true;
            continue;
        }
        parts.push(part);
    }
    parts.join(" ")
}

fn redact_url(url: &str) -> String {
    if url.starts_with("file://") {
        "file://[local-test]".into()
    } else if url.trim().is_empty() {
        "[not-configured]".into()
    } else {
        "[redacted-url]".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_empty_namespace() {
        let err = RemoteTursoConfig::new()
            .insert_namespace(
                "",
                RemoteTursoNamespaceConfig::remote("libsql://db", "token"),
            )
            .unwrap_err();

        assert_eq!(err.code, "VALIDATION_ERROR");
    }

    #[test]
    fn diagnostics_hide_urls_and_tokens() {
        let provider = RemoteTursoProvider::new_remote(
            "@acme",
            "libsql://sensitive-account.turso.io?token=abc",
            "secret-token",
        )
        .unwrap();

        let diagnostics = provider.diagnostics().unwrap().to_string();

        assert!(diagnostics.contains("[redacted-url]"));
        assert!(!diagnostics.contains("sensitive-account"));
        assert!(!diagnostics.contains("secret-token"));
    }

    #[test]
    fn redacts_operational_error_urls_and_bearer_tokens() {
        let err = remote_error(
            "failed to reach https://example.turso.io with Authorization Bearer secret-token",
        );

        assert_eq!(err.code, REMOTE_ERROR);
        assert!(!err.message.contains("example.turso.io"));
        assert!(!err.message.contains("secret-token"));
    }
}
