//! WASI capability configuration (wasmtime standalone path — story 07.05).

use std::collections::HashMap;

use edger_core::WorkerConfig;

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
        // Inject all operator-declared env into the wasm worker (trusted server side).
        let env = config
            .env
            .iter()
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
    fn injects_all_declared_worker_env() {
        // Server-side workers are a trusted context: ALL operator-declared env is
        // injected, including secrets like DATABASE_URL. Browser exposure is gated
        // separately by the publicEnv allowlist (static_spa.rs), never here.
        let mut manifest = WorkerManifest {
            name: "wasm-env".into(),
            ..Default::default()
        };
        manifest.env = Some(HashMap::from([
            ("AWS_REGION".into(), "us-east-1".into()),
            ("DATABASE_URL".into(), "postgres://example".into()),
            ("GITHUB_TOKEN".into(), "hidden".into()),
            ("PUBLIC_FLAG".into(), "true".into()),
        ]));

        let config = parse_worker_config(&manifest);
        let wasi = WasiConfig::from_worker_config(&config);

        assert_eq!(
            wasi.env.get("PUBLIC_FLAG").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            wasi.env.get("DATABASE_URL").map(String::as_str),
            Some("postgres://example")
        );
        assert_eq!(
            wasi.env.get("AWS_REGION").map(String::as_str),
            Some("us-east-1")
        );
        assert_eq!(
            wasi.env.get("GITHUB_TOKEN").map(String::as_str),
            Some("hidden")
        );
        assert_eq!(wasi.env.len(), 4);
        assert!(wasi.allow_env);
    }
}
