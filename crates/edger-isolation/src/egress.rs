//! Egress guard hook of the persistent process backend (EDG-38 S2b).
//!
//! The guard service itself lives in `edger-orchestrator` (it needs the HTTP
//! client stack); this crate only creates the per-process Unix listener in
//! the process workdir and hands it to an [`EgressListenerSink`], the same
//! way `AppSecretResolver` hands the vault over. The sink future runs as a
//! task owned by the spawned process: it is aborted when the process is
//! dropped (recycle, TTL, LRU, shutdown), which closes the listener, and the
//! socket file disappears with the workdir.

use std::future::Future;
use std::pin::Pin;

use edger_core::{EgressSettings, IsolationError, WorkerConfig};

/// The host the worker puts in the URL of an egress call. It never resolves
/// (`.invalid`, RFC 6761): the call only works through the guard socket.
pub const EGRESS_GUARD_HOST: &str = "edger-egress.invalid";

// The two env names (and the reservation filter every backend shares) live
// in `guard_env`, which is compiled without the `multiproc` feature so the
// CLI bridge can use it too.
pub use crate::guard_env::{EGRESS_HOST_ENV, EGRESS_SOCKET_ENV};

/// Who owns one guard socket: the worker process behind it, plus the
/// version's egress policy.
#[derive(Clone, Debug)]
pub struct EgressListenerContext {
    pub worker: String,
    pub version: String,
    pub namespace: Option<String>,
    /// The instance id shown in `/metrics/stats` (`<pid>-<seq>`).
    pub process_id: String,
    /// The declared GENERAL egress (`Some` = the version asked for it with
    /// `egress.enabled`). `None` = the socket exists for a private seam only
    /// (the managed database): the sink must not serve general network
    /// calls on it. `limits` is never an authority.
    pub settings: Option<EgressSettings>,
    /// Limits (timeouts, caps, concurrency) the sink applies to every call
    /// on this socket. Always finite; it grants nothing: with `settings`
    /// `None` it is the default profile (`DEFAULT_EGRESS_*`).
    pub limits: EgressSettings,
}

/// The default limits profile (`DEFAULT_EGRESS_*` of the core): the limits
/// of a socket that carries no general egress declaration.
pub fn default_egress_limits() -> EgressSettings {
    EgressSettings {
        ports: edger_core::config::DEFAULT_EGRESS_PORTS.to_vec(),
        connect_timeout_ms: edger_core::config::DEFAULT_EGRESS_CONNECT_TIMEOUT_MS,
        headers_timeout_ms: edger_core::config::DEFAULT_EGRESS_HEADERS_TIMEOUT_MS,
        idle_timeout_ms: edger_core::config::DEFAULT_EGRESS_IDLE_TIMEOUT_MS,
        total_timeout_ms: edger_core::config::DEFAULT_EGRESS_TOTAL_TIMEOUT_MS,
        max_response_bytes: edger_core::config::DEFAULT_EGRESS_MAX_RESPONSE_BYTES,
        max_concurrent: edger_core::config::DEFAULT_EGRESS_MAX_CONCURRENT,
    }
}

/// The future a sink returns: runs until it is dropped or aborted.
pub type EgressServeFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Receives the listener of one worker process. `serve` must not block: it
/// returns the service future, which the backend spawns and aborts when the
/// process ends.
pub trait EgressListenerSink: Send + Sync {
    fn serve(
        &self,
        listener: tokio::net::UnixListener,
        context: EgressListenerContext,
    ) -> EgressServeFuture;
}

/// Aborts the spawned service task when dropped, so a process that ends (or
/// fails to finish its spawn) never leaves the guard task alive.
pub(crate) struct EgressTask(tokio::task::JoinHandle<()>);

impl EgressTask {
    pub(crate) fn spawn(future: EgressServeFuture) -> Self {
        Self(tokio::spawn(future))
    }
}

impl Drop for EgressTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What an isolate keeps to arm the guard on every process it spawns.
#[derive(Clone)]
pub(crate) struct EgressBinding {
    pub(crate) sink: std::sync::Arc<dyn EgressListenerSink>,
    pub(crate) worker: String,
    pub(crate) version: String,
    pub(crate) namespace: Option<String>,
}

/// One spawn's guard: the sink and the context (the process id is filled in
/// once the child exists).
pub(crate) struct EgressSpawn {
    pub(crate) sink: std::sync::Arc<dyn EgressListenerSink>,
    pub(crate) context: EgressListenerContext,
    /// The managed database origin the HOST validated (never read from the
    /// guest env, manifest or headers). `Some` = a private managed spawn:
    /// the guard socket MUST bind (no fallback) and the origin goes to the
    /// harness as the extra argument; `None` = the generic guard.
    pub(crate) managed_origin: Option<String>,
}

/// The private managed-database state of one isolate. It lives outside
/// `WorkerConfig` and is never serialized: only the host sets it, between
/// processes, from an origin it already validated. It is independent of
/// `with_egress` (the generic guard).
#[derive(Clone)]
pub(crate) struct ManagedDatabaseBinding {
    pub(crate) sink: std::sync::Arc<dyn EgressListenerSink>,
    pub(crate) worker: String,
    pub(crate) version: String,
    pub(crate) namespace: Option<String>,
    pub(crate) origin: String,
}

impl ManagedDatabaseBinding {
    /// Plan the spawn of a managed process: the manifest must deny general
    /// network (`allow_net == Some([])`; `None` would open `--allow-net`
    /// and a declared host list is a second path to the database), whatever
    /// `config.egress` says. The context carries general-egress authority
    /// only when the manifest declared it (`settings == config.egress`);
    /// the limits are the declared ones, else the default profile. The
    /// database origin/grant stays host-only and separate.
    pub(crate) fn plan_spawn(&self, config: &WorkerConfig) -> Result<EgressSpawn, IsolationError> {
        if !matches!(config.allow_net.as_deref(), Some([])) {
            return Err(IsolationError::new(
                "UDS_MANAGED_DATABASE_NET",
                "a managed database worker must declare allowNet: [] (no general network)",
            ));
        }
        Ok(EgressSpawn {
            sink: std::sync::Arc::clone(&self.sink),
            context: EgressListenerContext {
                worker: self.worker.clone(),
                version: self.version.clone(),
                namespace: self.namespace.clone(),
                process_id: String::new(),
                // General authority is the manifest's explicit declaration
                // only: `egress` Some -> Some (limits = its settings); None
                // -> None with the default limits. Limits never become
                // authority and `config.egress` is not rewritten.
                settings: config.egress.clone(),
                limits: config.egress.clone().unwrap_or_else(default_egress_limits),
            },
            managed_origin: Some(self.origin.clone()),
        })
    }
}

/// Host-side sanity of the origin handed to the setter: non-empty and no
/// whitespace or control characters (it travels as one process argument).
/// Its URL meaning is validated by the bootstrap with the native `URL`,
/// which fails closed.
pub(crate) fn check_managed_origin(origin: &str) -> Result<(), IsolationError> {
    if origin.is_empty() || origin.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(IsolationError::new(
            "UDS_MANAGED_DATABASE_ORIGIN",
            "managed database origin must be non-empty and free of whitespace and control characters",
        ));
    }
    Ok(())
}

/// Warn that a version with `egress.enabled` runs WITHOUT the guard (no
/// socket, no variables): once per worker and version for the life of the
/// process, not once per spawned process.
pub fn warn_egress_unavailable_once(worker: &str, version: &str, reason: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock, PoisonError};

    static WARNED: OnceLock<Mutex<HashSet<(String, String)>>> = OnceLock::new();
    let first = WARNED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert((worker.to_string(), version.to_string()));
    if first {
        tracing::warn!(
            worker = %worker,
            version = %version,
            "egress guard unavailable: {reason}; the worker starts without EDGER_EGRESS_SOCKET and EDGER_EGRESS_HOST"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edger_core::{parse_worker_config, WorkerManifest};

    struct NopSink;
    impl EgressListenerSink for NopSink {
        fn serve(
            &self,
            _: tokio::net::UnixListener,
            _: EgressListenerContext,
        ) -> EgressServeFuture {
            Box::pin(async {})
        }
    }

    fn binding() -> ManagedDatabaseBinding {
        ManagedDatabaseBinding {
            sink: std::sync::Arc::new(NopSink),
            worker: "w".into(),
            version: "1".into(),
            namespace: Some("ns".into()),
            origin: "http://sqld.internal:8080".into(),
        }
    }

    fn config(allow_net: Option<Vec<String>>) -> WorkerConfig {
        parse_worker_config(&WorkerManifest {
            name: "w".into(),
            allow_net,
            ..WorkerManifest::default()
        })
    }

    #[test]
    fn managed_plan_requires_empty_allow_net_and_carries_no_authority() {
        let plan = binding().plan_spawn(&config(Some(vec![]))).unwrap();
        assert!(plan.context.settings.is_none(), "limits are not authority");
        assert_eq!(plan.context.limits, default_egress_limits());
        assert_eq!(plan.context.namespace.as_deref(), Some("ns"));
        assert_eq!(
            plan.managed_origin.as_deref(),
            Some("http://sqld.internal:8080")
        );
        assert!(plan.context.limits.max_concurrent > 0);
        assert!(plan.context.limits.total_timeout_ms > 0);
    }

    #[test]
    fn managed_plan_refuses_absent_or_nonempty_allow_net() {
        for allow in [None, Some(vec!["sqld.internal:8080".to_string()])] {
            let err = binding().plan_spawn(&config(allow)).err().expect("refused");
            assert_eq!(err.code, "UDS_MANAGED_DATABASE_NET");
        }
    }

    #[test]
    fn declared_general_egress_is_preserved_and_does_not_skip_the_net_rule() {
        let mut declared = config(Some(vec![]));
        let mut limits = default_egress_limits();
        limits.max_concurrent = 3;
        declared.egress = Some(limits.clone());
        let plan = binding().plan_spawn(&declared).unwrap();
        assert_eq!(plan.context.settings, Some(limits.clone()));
        assert_eq!(plan.context.limits, limits);
        assert!(declared.egress.is_some(), "config.egress is not rewritten");
        declared.allow_net = Some(vec!["example.com".into()]);
        assert!(binding().plan_spawn(&declared).is_err());
    }

    #[test]
    fn origin_sanity_rejects_empty_and_control_values_only() {
        assert!(check_managed_origin("http://sqld.internal:8080").is_ok());
        // The URL meaning is the bootstrap's job: a bad URL passes here.
        assert!(check_managed_origin("not-a-url").is_ok());
        for bad in ["", "http://a\u{0}b", "http://a b", "http://a\nb"] {
            assert!(check_managed_origin(bad).is_err(), "{bad:?}");
        }
    }
}
