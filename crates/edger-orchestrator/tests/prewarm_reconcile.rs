use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};
use edger_orchestrator::{rescan_workers_and_prewarm, ManifestIndex};
use edger_worker::{IsolateFactory, PoolConfig, WorkerPool, WorkerState};

#[derive(Default)]
struct PreparingFactory {
    created: AtomicUsize,
    prepared: Arc<AtomicUsize>,
}

impl PreparingFactory {
    fn created_count(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }

    fn prepared_count(&self) -> usize {
        self.prepared.load(Ordering::SeqCst)
    }
}

impl IsolateFactory for PreparingFactory {
    fn create_isolate(&self, _worker_ref: &edger_core::WorkerRef) -> Box<dyn Isolate> {
        self.created.fetch_add(1, Ordering::SeqCst);
        Box::new(PreparingIsolate {
            prepared: Arc::clone(&self.prepared),
        })
    }
}

fn write_worker(root: &std::path::Path, name: &str, manifest: &str, files: &[(&str, &str)]) {
    let worker_dir = root.join(name);
    std::fs::create_dir(&worker_dir).unwrap();
    std::fs::write(worker_dir.join("manifest.yaml"), manifest).unwrap();
    for (file, contents) in files {
        std::fs::write(worker_dir.join(file), contents).unwrap();
    }
}

struct PreparingIsolate {
    prepared: Arc<AtomicUsize>,
}

#[async_trait]
impl Isolate for PreparingIsolate {
    async fn prepare(&mut self, _config: &WorkerConfig) -> Result<(), IsolationError> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(Bytes::from_static(b"ok")),
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(
            SerializedRequest {
                method: "GET".into(),
                uri: "/".into(),
                headers: vec![],
                body: None,
                request_id: "prewarm".into(),
                base_href: None,
            },
            config,
        )
        .await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }
}

// Mutation captured: leaving rescan as index-only work means no isolate is
// prepared until the first HTTP dispatch reaches the worker.
#[tokio::test]
async fn rescan_prewarms_min_processes_before_first_request() {
    let root = tempfile::TempDir::new().unwrap();
    write_worker(
        root.path(),
        "warm",
        r#"name: warm
entrypoint: index.ts
ttl: 30s
minProcesses: 2
maxProcesses: 3
"#,
        &[("index.ts", "Deno.serve(() => new Response('ok'));")],
    );

    let index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    let factory = Arc::new(PreparingFactory::default());
    let pool = WorkerPool::with_factory(PoolConfig::default(), factory.clone());

    let report = rescan_workers_and_prewarm(&index, &pool, false)
        .await
        .unwrap();

    assert_eq!(report.added, vec!["warm@latest"]);
    assert_eq!(factory.created_count(), 2);
    assert_eq!(factory.prepared_count(), 2);
    assert_eq!(pool.len(), 2);
    assert!(pool
        .worker_stats()
        .iter()
        .all(|worker| worker.state == WorkerState::Idle));
}

// Mutation captured: prewarming every minProcesses worker regardless of kind
// prepares static SPA index.html and Wasm modules as process workers.
#[tokio::test]
async fn rescan_prewarm_skips_static_spa_and_wasm_but_keeps_process_kinds() {
    let root = tempfile::TempDir::new().unwrap();
    write_worker(
        root.path(),
        "fetcher",
        r#"name: fetcher
entrypoint: index.ts
kind: fetch
ttl: 30s
minProcesses: 1
"#,
        &[("index.ts", "Deno.serve(() => new Response('fetch'));")],
    );
    write_worker(
        root.path(),
        "router",
        r#"name: router
entrypoint: index.ts
kind: routes
ttl: 30s
minProcesses: 1
"#,
        &[("index.ts", "export default { routes: {} };")],
    );
    write_worker(
        root.path(),
        "ssr",
        r#"name: ssr
kind: fullstack
adapter: hono
ssrEntrypoint: index.ts
ttl: 30s
minProcesses: 1
"#,
        &[("index.ts", "Deno.serve(() => new Response('ssr'));")],
    );
    write_worker(
        root.path(),
        "panel",
        r#"name: panel
entrypoint: index.html
kind: spa
ttl: 30s
minProcesses: 1
"#,
        &[(
            "index.html",
            "<!doctype html><html><head></head><body>panel</body></html>",
        )],
    );
    write_worker(
        root.path(),
        "module",
        r#"name: module
entrypoint: index.wasm
kind: wasm
ttl: 30s
minProcesses: 1
"#,
        &[("index.wasm", "\0asm\x01\0\0\0")],
    );

    let index = ManifestIndex::new();
    index.set_roots(vec![root.path().to_path_buf()]);
    let factory = Arc::new(PreparingFactory::default());
    let pool = WorkerPool::with_factory(PoolConfig::default(), factory.clone());

    let report = rescan_workers_and_prewarm(&index, &pool, false)
        .await
        .unwrap();

    assert_eq!(
        report.added,
        vec![
            "fetcher@latest",
            "module@latest",
            "panel@latest",
            "router@latest",
            "ssr@latest"
        ]
    );
    assert_eq!(factory.created_count(), 3);
    assert_eq!(factory.prepared_count(), 3);
    assert_eq!(pool.len(), 3);
    let mut warm_workers = pool
        .worker_stats()
        .into_iter()
        .map(|worker| worker.name)
        .collect::<Vec<_>>();
    warm_workers.sort();
    assert_eq!(warm_workers, vec!["fetcher", "router", "ssr"]);
}
