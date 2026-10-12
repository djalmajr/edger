//! Deno isolate backend (`--features deno`).
//!
//! The current backend uses the Deno CLI as a process-isolated execution bridge.
//! It keeps the Rust orchestrator and worker pool on the production path while
//! the embedded `deno_core` facade is still pending.

mod cli;
mod facade;

pub use crate::deno_bundle::{
    entry_needs_bundle, BundleFormat, DenoCliBundler, ModuleBundle, ModuleBundler,
};
pub use cli::DenoCliRunner;
pub use facade::DenoFacade;

use async_trait::async_trait;

use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};

use crate::static_spa::serve_static_spa;

fn not_impl(method: &str) -> IsolationError {
    IsolationError::new(
        "NOT_IMPLEMENTED",
        format!("DenoIsolate::{method} pending implementation"),
    )
}

/// JS/TS isolate backed by the Deno CLI bridge.
pub struct DenoIsolate {
    facade: DenoFacade,
    bundler: DenoCliBundler,
    runner: DenoCliRunner,
}

impl DenoIsolate {
    pub fn new(facade: DenoFacade) -> Self {
        Self {
            facade,
            bundler: DenoCliBundler::default(),
            runner: DenoCliRunner::default(),
        }
    }

    pub fn facade(&self) -> &DenoFacade {
        &self.facade
    }

    pub fn bundler(&self) -> &DenoCliBundler {
        &self.bundler
    }

    pub fn runner(&self) -> &DenoCliRunner {
        &self.runner
    }
}

/// Runs the synchronous Deno CLI runner on a blocking thread (EDG-14
/// amendment). The runner spawns `deno run` and may bundle the worker
/// (including the bounded `deno bundle` retry), so it must not block an
/// async runtime thread: while the thread is blocked, the wall-clock
/// limit in `execute_with_limits` cannot fire.
async fn execute_fetch_blocking(
    runner: DenoCliRunner,
    req: SerializedRequest,
    config: WorkerConfig,
) -> Result<SerializedResponse, IsolationError> {
    tokio::task::spawn_blocking(move || runner.execute_fetch(req, &config))
        .await
        .map_err(|err| {
            IsolationError::new(
                "DENO_BRIDGE_BLOCKING_FAILED",
                format!("deno CLI bridge blocking task did not complete: {err}"),
            )
        })?
}

#[async_trait]
impl Isolate for DenoIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        execute_fetch_blocking(self.runner.clone(), req, config.clone()).await
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        execute_fetch_blocking(self.runner.clone(), req, config.clone()).await
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        serve_static_spa(path, base_href, config)
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_wasm"))
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edger_core::{parse_worker_config, WorkerManifest};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    // EDG-14 amendment: the CLI runner is synchronous (it may bundle the
    // worker, with its bounded retry, and then spawns `deno run`). On a
    // `current_thread` runtime a task polling in parallel must make progress
    // while `execute_fetch` runs, because the runner runs on a blocking
    // thread. The fake `deno run` sleeps long enough to observe; without
    // `tokio::task::spawn_blocking` the sync call blocks the runtime thread
    // and the poller cannot tick at all.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn execute_fetch_runs_the_cli_runner_on_a_blocking_thread() {
        let root = tempfile::tempdir().unwrap();
        let worker_dir = root.path().join("bridge-worker");
        std::fs::create_dir_all(&worker_dir).unwrap();
        let entry = worker_dir.join("index.ts");
        std::fs::write(
            &entry,
            "import { message } from './lib.ts';\nexport default () => new Response(message);\n",
        )
        .unwrap();
        let lib = worker_dir.join("lib.ts");
        std::fs::write(&lib, "export const message = 'bridge-ok';\n").unwrap();
        let graph = serde_json::json!({
            "roots": [format!("file://{}", entry.display())],
            "modules": [
                {
                    "specifier": format!("file://{}", entry.display()),
                    "local": entry,
                    "dependencies": [
                        {"code": {"specifier": format!("file://{}", lib.display())}}
                    ]
                },
                {
                    "specifier": format!("file://{}", lib.display()),
                    "local": lib,
                    "dependencies": []
                }
            ]
        });
        // Fake deno: a valid graph for `info`, an instant `bundle`, and a
        // `run` that simulates a slow worker (sleeps 2 s, then answers the
        // bridge protocol).
        let script = format!(
            r#"#!/bin/sh
if [ "$1" = "info" ]; then
  printf '%s' '{graph}'
  exit 0
fi
if [ "$1" = "bundle" ]; then
  out=
  prev=
  for a in "$@"; do
    if [ "$prev" = "--output" ]; then out="$a"; fi
    prev="$a"
  done
  printf 'const bundled = true;\n' > "$out"
  exit 0
fi
sleep 2
printf '__EDGER_RESPONSE__{{"status":200,"headers":[["content-type","text/plain"]],"body":null}}\n'
exit 0
"#,
            graph = graph,
        );
        let fake = root.path().join("fake-deno");
        std::fs::write(&fake, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let manifest = WorkerManifest {
            name: "bridge-worker".into(),
            ..WorkerManifest::default()
        };
        let mut config = parse_worker_config(&manifest);
        config.worker_dir = Some(worker_dir);
        let request = SerializedRequest {
            method: "GET".into(),
            uri: "/".into(),
            headers: vec![],
            body: None,
            request_id: "req-blocking".into(),
            base_href: None,
        };

        let mut isolate = DenoIsolate {
            facade: DenoFacade::default(),
            bundler: DenoCliBundler::default(),
            runner: DenoCliRunner::new(fake.to_string_lossy().into_owned()),
        };

        let ticks = Arc::new(AtomicUsize::new(0));
        let poller_ticks = ticks.clone();
        let poller = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(5)).await;
                poller_ticks.fetch_add(1, Ordering::Relaxed);
            }
        });

        let result = tokio::time::timeout(
            Duration::from_secs(30),
            isolate.execute_fetch(request, &config),
        )
        .await
        .expect("the isolate call must finish within the outer budget")
        .expect("the bridge call must succeed");
        poller.abort();
        let _ = poller.await;

        assert_eq!(
            result.status, 200,
            "the fake worker must answer through the bridge"
        );
        let poller_progress = ticks.load(Ordering::Relaxed);
        assert!(
            poller_progress >= 100,
            "the poller made no progress while the CLI runner ran (the runtime thread was blocked): {poller_progress} ticks"
        );
    }
}
