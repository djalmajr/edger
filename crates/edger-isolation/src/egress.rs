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

use edger_core::EgressSettings;

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
    pub settings: EgressSettings,
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
