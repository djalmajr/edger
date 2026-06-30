//! WASI capability configuration (wasmtime standalone path — story 07.05).

use std::collections::HashMap;

use edger_core::{is_sensitive_env_key, WorkerConfig};

/// WASI sandbox capabilities for Wasm workers (defaults deny-all).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WasiConfig {
    pub allow_env: bool,
    pub allow_fs_read: bool,
    pub allow_fs_write: bool,
    pub allow_net: bool,
    pub allow_stdio: bool,
    pub env: HashMap<String, String>,
}

impl WasiConfig {
    pub fn deny_all() -> Self {
        Self::default()
    }

    pub fn from_worker_config(config: &WorkerConfig) -> Self {
        let env = config
            .env
            .iter()
            .filter(|(key, _)| !is_sensitive_env_key(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<HashMap<_, _>>();

        Self {
            allow_env: !env.is_empty(),
            env,
            ..Self::deny_all()
        }
    }

    pub fn is_restricted(&self) -> bool {
        !self.allow_env
            && !self.allow_fs_read
            && !self.allow_fs_write
            && !self.allow_net
            && !self.allow_stdio
            && self.env.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edger_core::{parse_worker_config, WorkerManifest};

    #[test]
    fn deny_all_is_restricted() {
        assert!(WasiConfig::deny_all().is_restricted());
    }

    #[test]
    fn filters_sensitive_worker_env() {
        let mut manifest = WorkerManifest {
            name: "wasm-env".into(),
            ..Default::default()
        };
        manifest.env = Some(HashMap::from([
            ("AWS_REGION".into(), "us-east-1".into()),
            ("DATABASE_URL".into(), "postgres://example".into()),
            ("DB_URL".into(), "postgres://example".into()),
            ("GITHUB_TOKEN".into(), "hidden".into()),
            ("OPENAI_API_KEY".into(), "hidden".into()),
            ("PUBLIC_FLAG".into(), "true".into()),
            ("ROOT_SECRET".into(), "hidden".into()),
            ("SERVICE_KEY".into(), "hidden".into()),
            ("ADMIN_PASSWORD".into(), "hidden".into()),
        ]));

        let config = parse_worker_config(&manifest);
        let wasi = WasiConfig::from_worker_config(&config);

        assert_eq!(
            wasi.env.get("PUBLIC_FLAG").map(String::as_str),
            Some("true")
        );
        assert!(!wasi.env.contains_key("AWS_REGION"));
        assert!(!wasi.env.contains_key("DATABASE_URL"));
        assert!(!wasi.env.contains_key("DB_URL"));
        assert!(!wasi.env.contains_key("GITHUB_TOKEN"));
        assert!(!wasi.env.contains_key("OPENAI_API_KEY"));
        assert!(!wasi.env.contains_key("ROOT_SECRET"));
        assert!(!wasi.env.contains_key("SERVICE_KEY"));
        assert!(!wasi.env.contains_key("ADMIN_PASSWORD"));
        assert!(wasi.allow_env);
    }
}
