//! EDG-55 (S2): `WorkerPool::fetch_worker_completed` - a short execution that
//! returns only after the guest really ran and finished: whole body in
//! memory (bounded), production confirmed clean, dispatch slot and isolate
//! guard already released.
//!
//! The isolate is a fake (the real Deno proof belongs to slice S4). Time is
//! paused where a deadline matters; no test sleeps for real.

mod helpers;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::config::{FullstackBasePath, FullstackConfig, WasmBackendConfig};
use edger_core::wire::StreamProductionState;
use edger_core::{
    create_worker_ref, AbandonedStream, ExecutionKind, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, StreamCompletion, StreamedResponse, WorkerConfig, WorkerRef,
    WorkerResponse,
};
use edger_worker::{IsolateFactory, WorkerError, WorkerPool, WorkerState};
use helpers::{default_pool_config, pool_with_factory, serialized_get, temp_worker_dir};
use tempfile::TempDir;
use tokio::sync::Notify;

// ---------------------------------------------------------------------------
// Fake isolate
// ---------------------------------------------------------------------------

/// What the body does after the scripted chunks.
#[derive(Clone, Copy)]
enum Tail {
    End,
    Error,
    Pending,
}

/// How the backend reports production (the completion signal and the shared
/// production-complete flag).
#[derive(Clone)]
enum Production {
    /// Whole answer buffered (WASM-like backend).
    Buffered(&'static [u8]),
    /// Pre-EDG-8 backend: no signal, no flag; the body end is the only trigger.
    NoSignals,
    /// Flag set and signal resolved `Completed` before the body is read.
    CleanBeforeEnd,
    /// The body ends FIRST; flag and `Completed` only when `gate` opens.
    CleanAfterGate,
    /// The producer reports `Incomplete` (end without clean production).
    Incomplete,
    /// The reader cut the body at the max-duration limit (clean-looking EOF).
    MaxDurationCut,
    /// The reader cut the body at the per-frame timeout (clean-looking EOF).
    FrameTimeoutCut,
    /// The signal never resolves.
    NeverSignals,
}

#[derive(Clone)]
struct Behavior {
    chunks: Vec<&'static [u8]>,
    /// Endless producer: chunks of this size, forever, counted in `polled`.
    endless: Option<usize>,
    /// Time the first chunk takes to appear after the head.
    first_chunk_delay: Duration,
    gate: Arc<Notify>,
    /// Time the isolate takes to answer with the response head.
    head_delay: Duration,
    /// The endless producer errors (`FAKE_SPIN_LIMIT`) after this many polls,
    /// so a collector that never times out fails instead of hanging.
    poll_cap: usize,
    polled: Arc<AtomicUsize>,
    production: Production,
    tail: Tail,
    /// The buffered call reports the host's truncation mark (the harness cut
    /// the body at its byte cap): `take_response_truncation` answers `true`
    /// once after the call.
    truncated_buffered: bool,
}

impl Behavior {
    fn new(production: Production) -> Self {
        Self {
            chunks: vec![b"hello ", b"world"],
            endless: None,
            first_chunk_delay: Duration::ZERO,
            gate: Arc::new(Notify::new()),
            head_delay: Duration::ZERO,
            poll_cap: usize::MAX,
            polled: Arc::new(AtomicUsize::new(0)),
            production,
            tail: Tail::End,
            truncated_buffered: false,
        }
    }

    fn truncated_buffered(mut self) -> Self {
        self.truncated_buffered = true;
        self
    }

    fn head_delay(mut self, delay: Duration) -> Self {
        self.head_delay = delay;
        self
    }

    fn first_chunk_delay(mut self, delay: Duration) -> Self {
        self.first_chunk_delay = delay;
        self
    }

    fn poll_cap(mut self, cap: usize) -> Self {
        self.poll_cap = cap;
        self
    }

    fn chunks(mut self, chunks: Vec<&'static [u8]>) -> Self {
        self.chunks = chunks;
        self
    }

    fn endless(mut self, chunk_size: usize) -> Self {
        self.chunks = vec![];
        self.endless = Some(chunk_size);
        self
    }

    fn tail(mut self, tail: Tail) -> Self {
        self.tail = tail;
        self
    }
}

struct ScriptBody {
    chunks: VecDeque<Bytes>,
    delay: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
    endless: Option<usize>,
    error_sent: bool,
    poll_cap: usize,
    polled: Arc<AtomicUsize>,
    tail: Tail,
}

impl futures_core::Stream for ScriptBody {
    type Item = Result<Bytes, IsolationError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::future::Future;
        use std::task::Poll;
        if let Some(delay) = self.delay.as_mut() {
            if delay.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.delay = None;
        }
        if let Some(chunk) = self.chunks.pop_front() {
            self.polled.fetch_add(1, Ordering::SeqCst);
            return Poll::Ready(Some(Ok(chunk)));
        }
        if let Some(size) = self.endless {
            if self.polled.fetch_add(1, Ordering::SeqCst) >= self.poll_cap {
                return Poll::Ready(Some(Err(IsolationError::new(
                    "FAKE_SPIN_LIMIT",
                    "the collector never timed out",
                ))));
            }
            return Poll::Ready(Some(Ok(Bytes::from(vec![b'x'; size]))));
        }
        match self.tail {
            Tail::End => Poll::Ready(None),
            Tail::Error if !self.error_sent => {
                self.error_sent = true;
                Poll::Ready(Some(Err(IsolationError::new(
                    "BODY_STREAM_ERROR",
                    "body failed mid-stream",
                ))))
            }
            Tail::Error => Poll::Ready(None),
            Tail::Pending => Poll::Pending,
        }
    }
}

#[derive(Default)]
struct Shared {
    created: AtomicUsize,
    executed: AtomicUsize,
    script: Mutex<HashMap<String, Behavior>>,
}

struct FakeIsolate {
    shared: Arc<Shared>,
    truncation: bool,
}

impl FakeIsolate {
    fn behavior(&self, req: &SerializedRequest) -> Behavior {
        let path = req.uri.split('?').next().unwrap_or("").to_string();
        self.shared
            .script
            .lock()
            .expect("script lock")
            .get(&path)
            .cloned()
            .unwrap_or_else(|| Behavior::new(Production::CleanBeforeEnd))
    }
}

#[async_trait]
impl Isolate for FakeIsolate {
    async fn execute_fetch_stream(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.shared.executed.fetch_add(1, Ordering::SeqCst);
        let behavior = self.behavior(&req);
        if !behavior.head_delay.is_zero() {
            tokio::time::sleep(behavior.head_delay).await;
        }
        if let Production::Buffered(body) = behavior.production {
            return Ok(WorkerResponse::Buffered(SerializedResponse {
                status: 200,
                headers: vec![("x-fake".into(), "buffered".into())],
                body: Some(Bytes::from_static(body)),
            }));
        }
        let state = Arc::new(StreamProductionState::default());
        let (completed, production_complete): (
            Option<edger_core::CompletionSignal>,
            Option<Arc<StreamProductionState>>,
        ) = match behavior.production.clone() {
            Production::Buffered(_) | Production::NoSignals => (None, None),
            Production::CleanBeforeEnd => {
                state.mark_complete();
                (
                    Some(Box::pin(async { StreamCompletion::Completed })),
                    Some(state),
                )
            }
            Production::CleanAfterGate => {
                let flag = Arc::clone(&state);
                let gate = Arc::clone(&behavior.gate);
                (
                    Some(Box::pin(async move {
                        gate.notified().await;
                        flag.mark_complete();
                        StreamCompletion::Completed
                    })),
                    Some(state),
                )
            }
            Production::Incomplete => (
                Some(Box::pin(async { StreamCompletion::Incomplete })),
                Some(state),
            ),
            Production::MaxDurationCut => {
                state.mark_max_duration_complete(AbandonedStream::Cancelled);
                (
                    Some(Box::pin(async {
                        StreamCompletion::MaxDuration(AbandonedStream::Cancelled)
                    })),
                    Some(state),
                )
            }
            Production::FrameTimeoutCut => {
                state.mark_frame_timeout_complete(AbandonedStream::Cancelled);
                (
                    Some(Box::pin(async {
                        StreamCompletion::FrameTimeout(AbandonedStream::Cancelled)
                    })),
                    Some(state),
                )
            }
            Production::NeverSignals => (Some(Box::pin(std::future::pending())), Some(state)),
        };
        Ok(WorkerResponse::Streamed(StreamedResponse {
            status: 200,
            headers: vec![("x-fake".into(), "streamed".into())],
            body: Box::pin(ScriptBody {
                chunks: behavior
                    .chunks
                    .iter()
                    .map(|chunk| Bytes::from_static(chunk))
                    .collect(),
                delay: (!behavior.first_chunk_delay.is_zero())
                    .then(|| Box::pin(tokio::time::sleep(behavior.first_chunk_delay))),
                endless: behavior.endless,
                error_sent: false,
                poll_cap: behavior.poll_cap,
                polled: Arc::clone(&behavior.polled),
                tail: behavior.tail,
            }),
            completed,
            production_complete,
            max_duration_elapsed_ms: None,
            frame_timeout_elapsed_ms: None,
        }))
    }

    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.shared.executed.fetch_add(1, Ordering::SeqCst);
        let behavior = self.behavior(&req);
        self.truncation |= behavior.truncated_buffered;
        let body = match behavior.production {
            Production::Buffered(body) => Bytes::from_static(body),
            _ => Bytes::from_static(b"fetch"),
        };
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("x-fake".into(), "buffered".into())],
            body: Some(body),
        })
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_fetch(req, config).await
    }

    fn take_response_truncation(&mut self) -> bool {
        std::mem::take(&mut self.truncation)
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.shared.executed.fetch_add(1, Ordering::SeqCst);
        Err(IsolationError::new("NOT_IMPLEMENTED", "spa"))
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.shared.executed.fetch_add(1, Ordering::SeqCst);
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("x-fake".into(), "wasm-api".into())],
            body: Some(Bytes::from_static(b"wasm-api")),
        })
    }
}

struct FakeFactory {
    shared: Arc<Shared>,
}

impl IsolateFactory for FakeFactory {
    fn create_isolate(&self, _worker_ref: &WorkerRef) -> Box<dyn Isolate> {
        self.shared.created.fetch_add(1, Ordering::SeqCst);
        Box::new(FakeIsolate {
            shared: Arc::clone(&self.shared),
            truncation: false,
        })
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    _dir: TempDir,
    pool: WorkerPool,
    shared: Arc<Shared>,
    worker_ref: WorkerRef,
}

impl Fixture {
    /// `maxProcesses: 1` and a 2 s worker `timeout`.
    fn new(name: &str, ttl: &str) -> Self {
        Self::with_timeout(name, ttl, "2s", 2000)
    }

    fn with_timeout(name: &str, ttl: &str, timeout: &str, timeout_ms: u64) -> Self {
        let manifest_yaml =
            format!("name: {name}\nttl: {ttl}\nmaxProcesses: 1\ntimeout: {timeout}\n");
        let (dir, config, manifest) = temp_worker_dir(&manifest_yaml);
        assert_eq!(config.timeout_ms, timeout_ms, "fixture timeout parsed");
        assert_eq!(config.max_processes, 1, "fixture maxProcesses parsed as 1");
        let worker_ref =
            create_worker_ref(dir.path().to_path_buf(), manifest).expect("valid manifest");
        let shared = Arc::new(Shared::default());
        let pool = pool_with_factory(
            Arc::new(FakeFactory {
                shared: Arc::clone(&shared),
            }),
            default_pool_config(),
        );
        Self {
            _dir: dir,
            pool,
            shared,
            worker_ref,
        }
    }

    fn script(&self, path: &str, behavior: Behavior) -> Behavior {
        self.shared
            .script
            .lock()
            .expect("script lock")
            .insert(path.to_string(), behavior.clone());
        behavior
    }

    async fn completed(
        &self,
        path: &str,
        max_body_bytes: usize,
    ) -> Result<edger_worker::CompletedResponse, WorkerError> {
        self.pool
            .fetch_worker_completed(&self.worker_ref, serialized_get(path), None, max_body_bytes)
            .await
    }

    fn executed(&self) -> usize {
        self.shared.executed.load(Ordering::SeqCst)
    }

    fn created(&self) -> usize {
        self.shared.created.load(Ordering::SeqCst)
    }

    /// Proof that the slot is free: every instance is `Idle`, has no active
    /// request, its dispatch lock is acquirable right now, and the group
    /// metrics report no active process.
    fn assert_slot_free(&self) {
        let group = self
            .pool
            .worker_group(&self.worker_ref)
            .expect("the worker group is cached");
        for instance in group.instances_snapshot() {
            assert_eq!(instance.state(), WorkerState::Idle, "instance is idle");
            assert!(
                instance.active_request_metrics().is_none(),
                "no active request left on the instance"
            );
            assert!(
                instance.dispatch_lock().try_lock_owned().is_ok(),
                "the dispatch lock is free"
            );
        }
        let metrics = self.pool.get_metrics();
        let group_metrics = metrics
            .worker_groups
            .iter()
            .find(|group| group.name == self.worker_ref.name)
            .expect("group metrics");
        assert_eq!(group_metrics.active_processes, 0, "pool active count is 0");
    }
}

fn is_failed(err: &WorkerError) -> bool {
    matches!(err, WorkerError::CompletedStreamFailed)
}

// ---------------------------------------------------------------------------
// Semantics
// ---------------------------------------------------------------------------

// Buffered backend: returns directly, with the slot free (persistent worker
// and ephemeral `ttl: 0` worker).
#[tokio::test(start_paused = true)]
async fn buffered_backend_returns_directly() {
    let fx = Fixture::new("buffered-persistent", "60");
    fx.script("/b", Behavior::new(Production::Buffered(b"buffered-body")));

    let res = fx.completed("/b", 1024).await.expect("buffered answer");

    assert_eq!(res.status, 200);
    assert_eq!(res.body, Bytes::from_static(b"buffered-body"));
    assert_eq!(
        res.headers,
        vec![("x-fake".to_string(), "buffered".to_string())]
    );
    fx.assert_slot_free();
}

#[tokio::test(start_paused = true)]
async fn ephemeral_ttl_zero_returns_directly() {
    let fx = Fixture::new("buffered-ephemeral", "0");
    fx.script("/b", Behavior::new(Production::Buffered(b"eph")));

    let res = fx.completed("/b", 1024).await.expect("ephemeral answer");

    assert_eq!(res.body, Bytes::from_static(b"eph"));
    assert_eq!(fx.executed(), 1, "the guest ran once");
}

// A buffered call the HOST cut at its byte cap (ephemeral `ttl: 0` Deno
// worker) is not a complete execution: the completed entry refuses it, the
// ordinary request keeps the truncated body, and the control below the cap
// passes. The mark is consumed by each dispatch, so it never leaks into the
// next one.
#[tokio::test(start_paused = true)]
async fn ephemeral_ttl_zero_cut_answer_is_refused_only_for_the_completed_entry() {
    let fx = Fixture::new("buffered-cut", "0");
    fx.script(
        "/cut",
        Behavior::new(Production::Buffered(b"cut-body")).truncated_buffered(),
    );
    fx.script("/ok", Behavior::new(Production::Buffered(b"ok-body")));

    let err = fx.completed("/cut", 1024).await.expect_err("cut answer");
    assert!(is_failed(&err), "{err:?}");
    assert_eq!(
        fx.executed(),
        1,
        "the guest ran: the answer is what is refused"
    );

    // The ordinary request of the same worker is unchanged.
    let res = fx
        .pool
        .fetch_worker(&fx.worker_ref, serialized_get("/cut"), None)
        .await
        .expect("an ordinary request keeps the truncated body");
    assert_eq!(res.status, 200);
    assert_eq!(res.body, Some(Bytes::from_static(b"cut-body")));

    // Control below the cap, after the cut one and after an ordinary request
    // that left its mark unread by the entry: no leak.
    let res = fx.completed("/ok", 1024).await.expect("control");
    assert_eq!(res.body, Bytes::from_static(b"ok-body"));
    assert_eq!(fx.executed(), 3);
}

#[tokio::test(start_paused = true)]
async fn buffered_body_over_limit_is_refused() {
    let fx = Fixture::new("buffered-large", "60");
    fx.script("/b", Behavior::new(Production::Buffered(b"0123456789")));

    let err = fx.completed("/b", 9).await.expect_err("over the limit");
    assert!(matches!(
        err,
        WorkerError::CompletedBodyTooLarge { limit: 9 }
    ));
    // Control: exactly at the limit passes.
    let res = fx.completed("/b", 10).await.expect("at the limit");
    assert_eq!(res.body.len(), 10);
}

// Slot proof: with `maxProcesses: 1`, when `Ok` is returned the pool shows 0
// active and the very next call gets THE SAME instance without waiting.
#[tokio::test(start_paused = true)]
async fn clean_stream_returns_with_slot_already_free() {
    let fx = Fixture::new("slot-free", "60");
    fx.script("/s", Behavior::new(Production::CleanBeforeEnd));

    let res = fx.completed("/s", 1024).await.expect("clean stream");

    assert_eq!(res.status, 200);
    assert_eq!(res.body, Bytes::from_static(b"hello world"));
    assert_eq!(
        res.headers,
        vec![("x-fake".to_string(), "streamed".to_string())]
    );
    fx.assert_slot_free();

    let started = tokio::time::Instant::now();
    let second = tokio::time::timeout(Duration::from_millis(1), fx.completed("/s", 1024))
        .await
        .expect("the next call must not wait for a late release")
        .expect("second call");
    assert_eq!(second.body, Bytes::from_static(b"hello world"));
    assert_eq!(started.elapsed(), Duration::ZERO, "no virtual time elapsed");
    assert_eq!(fx.created(), 1, "the same process served both calls");
    fx.assert_slot_free();
}

// Order: the body ends FIRST and `completed` only afterwards; the call must
// not return at the body end.
#[tokio::test(start_paused = true)]
async fn waits_for_completed_signal_that_follows_the_body_end() {
    let fx = Fixture::new("order", "60");
    let behavior = fx.script("/late", Behavior::new(Production::CleanAfterGate));

    let handle = {
        let pool = fx.pool.clone();
        let worker_ref = fx.worker_ref.clone();
        tokio::spawn(async move {
            pool.fetch_worker_completed(&worker_ref, serialized_get("/late"), None, 1024)
                .await
        })
    };
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        behavior.polled.load(Ordering::SeqCst),
        2,
        "both chunks were read, the body ended"
    );
    assert!(
        !handle.is_finished(),
        "returned at the body end, before the completed signal"
    );

    behavior.gate.notify_one();
    let res = handle
        .await
        .expect("join")
        .expect("completes after the signal");

    assert_eq!(res.body, Bytes::from_static(b"hello world"));
    fx.assert_slot_free();
}

// Pre-EDG-8 backend (no signal, no flag): the body end is the trigger.
#[tokio::test(start_paused = true)]
async fn backend_without_signals_completes_at_the_body_end() {
    let fx = Fixture::new("legacy", "60");
    fx.script("/l", Behavior::new(Production::NoSignals));

    let res = fx.completed("/l", 1024).await.expect("legacy backend");

    assert_eq!(res.body, Bytes::from_static(b"hello world"));
    fx.assert_slot_free();
}

// Assets served by the host never run the guest.
#[tokio::test(start_paused = true)]
async fn static_spa_is_not_guest_and_does_not_execute() {
    let fx = Fixture::new("spa", "60");

    let err = fx
        .pool
        .fetch_worker_completed(
            &fx.worker_ref,
            serialized_get("/index.html"),
            Some(ExecutionKind::StaticSpa { inject_base: false }),
            1024,
        )
        .await
        .expect_err("static-spa is host-served");

    assert!(matches!(err, WorkerError::CompletedNotGuest));
    assert_eq!(fx.executed(), 0, "the guest never ran");
    assert_eq!(fx.created(), 0, "no isolate was even created");
}

fn wasm_fullstack_fixture(name: &str) -> Fixture {
    let mut fx = Fixture::new(name, "60");
    let client = fx._dir.path().join("client");
    std::fs::create_dir_all(&client).expect("client dir");
    std::fs::write(client.join("index.html"), "<html>spa</html>").expect("index");
    let kind = ExecutionKind::Fullstack {
        adapter: "wasm".into(),
    };
    fx.worker_ref.kind = kind.clone();
    fx.worker_ref.config.kind = Some(kind);
    fx.worker_ref.config.fullstack = Some(FullstackConfig {
        adapter: "wasm".into(),
        ssr_entrypoint: None,
        client_dir: Some("client".into()),
        asset_prefixes: vec![],
        base_path: FullstackBasePath::Auto,
        wasm_backend: Some(WasmBackendConfig {
            entrypoint: "backend.wasm".into(),
            api_prefix: "/api".into(),
        }),
    });
    fx
}

#[tokio::test(start_paused = true)]
async fn fullstack_wasm_asset_is_not_guest_and_api_route_executes() {
    let fx = wasm_fullstack_fixture("fullstack-wasm");

    let asset = fx
        .pool
        .fetch_worker_completed(&fx.worker_ref, serialized_get("/"), None, 1024)
        .await
        .expect_err("the SPA page is host-served");
    assert!(matches!(asset, WorkerError::CompletedNotGuest));
    assert_eq!(fx.executed(), 0, "the asset never ran the guest");

    let api = fx
        .pool
        .fetch_worker_completed(&fx.worker_ref, serialized_get("/api/ping"), None, 1024)
        .await
        .expect("the API route executes the guest");
    assert_eq!(api.status, 200);
    assert_eq!(api.body, Bytes::from_static(b"wasm-api"));
    assert_eq!(fx.executed(), 1, "the guest ran for the API route only");
}

#[tokio::test(start_paused = true)]
async fn fullstack_js_asset_is_not_guest_and_does_not_execute() {
    let mut fx = Fixture::new("fullstack-js", "60");
    let assets = fx._dir.path().join("client").join("assets");
    std::fs::create_dir_all(&assets).expect("assets dir");
    std::fs::write(assets.join("app.js"), "console.log(1)").expect("asset");
    let kind = ExecutionKind::Fullstack {
        adapter: "vite".into(),
    };
    fx.worker_ref.kind = kind.clone();
    fx.worker_ref.config.kind = Some(kind);
    fx.worker_ref.config.fullstack = Some(FullstackConfig {
        adapter: "vite".into(),
        ssr_entrypoint: None,
        client_dir: Some("client".into()),
        asset_prefixes: vec!["/assets".into()],
        base_path: FullstackBasePath::Auto,
        wasm_backend: None,
    });

    let err = fx
        .pool
        .fetch_worker_completed(&fx.worker_ref, serialized_get("/assets/app.js"), None, 1024)
        .await
        .expect_err("a fullstack asset is host-served");

    assert!(matches!(err, WorkerError::CompletedNotGuest));
    assert_eq!(fx.executed(), 0, "the guest never ran");
    assert_eq!(fx.created(), 0, "no isolate was created");
}

// A JS `fullstack` worker (`hono` adapter: the uri is not rewritten) with
// `ttl: 0` runs through the buffered fullstack
// dispatch (`execute_fetch` of the same isolate), so the host's truncation
// mark of that call refuses the completed entry exactly like a plain
// `ttl: 0` fetch worker; the ordinary request keeps the answer.
#[tokio::test(start_paused = true)]
async fn ephemeral_fullstack_cut_answer_is_refused_only_for_the_completed_entry() {
    let mut fx = Fixture::new("fullstack-cut", "0");
    let kind = ExecutionKind::Fullstack {
        adapter: "hono".into(),
    };
    fx.worker_ref.kind = kind.clone();
    fx.worker_ref.config.kind = Some(kind);
    fx.worker_ref.config.fullstack = Some(FullstackConfig {
        adapter: "hono".into(),
        ssr_entrypoint: Some("server.js".into()),
        client_dir: None,
        asset_prefixes: vec![],
        base_path: FullstackBasePath::Auto,
        wasm_backend: None,
    });
    fx.script(
        "/page",
        Behavior::new(Production::Buffered(b"ssr-cut")).truncated_buffered(),
    );
    fx.script("/ok", Behavior::new(Production::Buffered(b"ssr-ok")));

    let err = fx.completed("/page", 1024).await.expect_err("cut answer");
    assert!(is_failed(&err), "{err:?}");
    assert_eq!(
        fx.executed(),
        1,
        "the guest ran: the answer is what is refused"
    );

    // The ordinary request of the same worker is unchanged.
    let res = fx
        .pool
        .fetch_worker(&fx.worker_ref, serialized_get("/page"), None)
        .await
        .expect("an ordinary request keeps the truncated body");
    assert_eq!(res.status, 200);
    assert_eq!(res.body, Some(Bytes::from_static(b"ssr-cut")));

    // Control without a mark.
    let res = fx.completed("/ok", 1024).await.expect("control");
    assert_eq!(res.body, Bytes::from_static(b"ssr-ok"));
    assert_eq!(fx.executed(), 3);
}

// Backpressure: a body over the limit is not accumulated past one chunk, the
// stream is not read again and the call never returns a partial response.
#[tokio::test(start_paused = true)]
async fn body_over_limit_fails_without_buffering_past_one_chunk() {
    let fx = Fixture::new("too-large", "60");
    let behavior = fx.script(
        "/big",
        Behavior::new(Production::CleanBeforeEnd).endless(1000),
    );

    let err = fx
        .completed("/big", 2500)
        .await
        .expect_err("over the limit");

    assert!(matches!(
        err,
        WorkerError::CompletedBodyTooLarge { limit: 2500 }
    ));
    let polled = behavior.polled.load(Ordering::SeqCst);
    assert!(
        polled * 1000 <= 2500 + 1000,
        "read {polled} chunks of 1000 bytes for a 2500 byte limit"
    );
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        behavior.polled.load(Ordering::SeqCst),
        polled,
        "the body is not polled after the limit is crossed"
    );

    // The abandoned process is recycled; the next call still works.
    fx.script("/ok", Behavior::new(Production::CleanBeforeEnd));
    let next = fx.completed("/ok", 1024).await.expect("next call");
    assert_eq!(next.body, Bytes::from_static(b"hello world"));
}

#[tokio::test(start_paused = true)]
async fn body_exactly_at_the_limit_is_accepted() {
    let fx = Fixture::new("at-limit", "60");
    fx.script(
        "/edge",
        Behavior::new(Production::CleanBeforeEnd).chunks(vec![b"12345", b"67890"]),
    );

    let res = fx.completed("/edge", 10).await.expect("exactly the limit");
    assert_eq!(res.body, Bytes::from_static(b"1234567890"));

    let err = fx.completed("/edge", 9).await.expect_err("one byte over");
    assert!(matches!(
        err,
        WorkerError::CompletedBodyTooLarge { limit: 9 }
    ));
}

// Frame error / end without clean production / reader cut: fail closed.
#[tokio::test(start_paused = true)]
async fn mid_stream_error_fails_closed() {
    let fx = Fixture::new("frame-error", "60");
    fx.script(
        "/e",
        Behavior::new(Production::CleanBeforeEnd).tail(Tail::Error),
    );

    let err = fx.completed("/e", 1024).await.expect_err("frame error");
    assert!(is_failed(&err), "got {err:?}");
}

#[tokio::test(start_paused = true)]
async fn end_without_clean_production_fails_closed() {
    let fx = Fixture::new("incomplete", "60");
    fx.script("/i", Behavior::new(Production::Incomplete));

    let err = fx
        .completed("/i", 1024)
        .await
        .expect_err("incomplete production");
    assert!(is_failed(&err), "got {err:?}");
    // Control: the SAME body with a clean production is accepted.
    fx.script("/c", Behavior::new(Production::CleanBeforeEnd));
    assert!(fx.completed("/c", 1024).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn reader_cut_looks_like_a_clean_end_but_is_refused() {
    let fx = Fixture::new("cut", "60");
    fx.script("/m", Behavior::new(Production::MaxDurationCut));

    let err = fx
        .completed("/m", 1024)
        .await
        .expect_err("a truncated body is never a response");
    assert!(is_failed(&err), "got {err:?}");
}

// The per-frame-timeout cut ends the body the same clean-looking way: it must
// fail closed (never `Ok` with a truncated body) and leave the slot free (the
// cut reuses the process), with a clean control on the same worker.
#[tokio::test(start_paused = true)]
async fn frame_timeout_cut_looks_like_a_clean_end_but_is_refused() {
    let fx = Fixture::new("frame-cut", "60");
    fx.script("/f", Behavior::new(Production::FrameTimeoutCut));
    fx.script("/c", Behavior::new(Production::CleanBeforeEnd));

    let err = fx
        .completed("/f", 1024)
        .await
        .expect_err("a truncated body is never a response");

    assert!(is_failed(&err), "got {err:?}");
    fx.assert_slot_free();
    assert_eq!(fx.created(), 1, "the cut reused the process");
    // Control: a clean production on the same worker is accepted.
    let ok = fx.completed("/c", 1024).await.expect("clean control");
    assert_eq!(ok.body, Bytes::from_static(b"hello world"));
    assert_eq!(fx.created(), 1, "same process served the control");
}

// R64 #1: an endless stream of EMPTY chunks that are always immediately ready
// never reaches the byte limit and never returns `Pending`, and `timeout_at`
// polls the stream before its timer. The collector must still enforce the
// deadline between chunks. Real clock (a paused clock cannot advance while the
// task never idles); the fake errors with `FAKE_SPIN_LIMIT` after `poll_cap`
// polls so a collector that never times out FAILS instead of hanging.
#[tokio::test]
async fn endless_ready_empty_chunks_time_out_and_release_the_slot() {
    let fx = Fixture::with_timeout("spin-empty", "60", "200ms", 200);
    let behavior = fx.script(
        "/spin",
        Behavior::new(Production::CleanBeforeEnd)
            .endless(0)
            .poll_cap(40_000_000),
    );

    let started = std::time::Instant::now();
    let err = fx.completed("/spin", 1024).await.expect_err("deadline");

    assert!(matches!(err, WorkerError::CompletedTimeout), "got {err:?}");
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "not before the worker timeout"
    );
    assert!(
        behavior.polled.load(Ordering::SeqCst) < 40_000_000,
        "stopped by the deadline, not by the fake's spin limit"
    );
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    fx.assert_slot_free();
}

// R64 #2: the body deadline starts when the head arrives, not at the call.
// Head after 1.5 s + first chunk 1 s later = 2.5 s end to end with a 2 s
// `timeout`: accepted, because the body took 1 s after the head.
#[tokio::test(start_paused = true)]
async fn slow_head_does_not_consume_the_body_budget() {
    let fx = Fixture::new("slow-head-ok", "60");
    fx.script(
        "/h",
        Behavior::new(Production::CleanBeforeEnd)
            .head_delay(Duration::from_millis(1500))
            .first_chunk_delay(Duration::from_millis(1000)),
    );

    let started = tokio::time::Instant::now();
    let res = fx
        .completed("/h", 1024)
        .await
        .expect("body within the timeout after the head");

    assert_eq!(res.body, Bytes::from_static(b"hello world"));
    assert_eq!(started.elapsed(), Duration::from_millis(2500));
    fx.assert_slot_free();
}

// A body that outlasts the `timeout` AFTER the head still times out, exactly
// `timeout` after the head arrived (1.5 s head + 2 s body budget).
#[tokio::test(start_paused = true)]
async fn body_slower_than_the_timeout_after_the_head_times_out() {
    let fx = Fixture::new("slow-head-late-body", "60");
    fx.script(
        "/h",
        Behavior::new(Production::CleanBeforeEnd)
            .head_delay(Duration::from_millis(1500))
            .first_chunk_delay(Duration::from_millis(2500)),
    );

    let started = tokio::time::Instant::now();
    let err = fx.completed("/h", 1024).await.expect_err("deadline");

    assert!(matches!(err, WorkerError::CompletedTimeout), "got {err:?}");
    assert_eq!(started.elapsed(), Duration::from_millis(3500));
}

// Deadline = worker timeout (2 s here).
#[tokio::test(start_paused = true)]
async fn body_that_never_ends_times_out() {
    let fx = Fixture::new("timeout-body", "60");
    fx.script(
        "/p",
        Behavior::new(Production::CleanBeforeEnd).tail(Tail::Pending),
    );

    let started = tokio::time::Instant::now();
    let err = fx.completed("/p", 1024).await.expect_err("deadline");

    assert!(matches!(err, WorkerError::CompletedTimeout), "got {err:?}");
    assert_eq!(started.elapsed(), Duration::from_secs(2), "worker timeout");
}

#[tokio::test(start_paused = true)]
async fn release_that_never_comes_times_out() {
    let fx = Fixture::new("timeout-release", "60");
    fx.script("/n", Behavior::new(Production::NeverSignals));

    let started = tokio::time::Instant::now();
    let err = fx.completed("/n", 1024).await.expect_err("deadline");

    assert!(matches!(err, WorkerError::CompletedTimeout), "got {err:?}");
    assert_eq!(started.elapsed(), Duration::from_secs(2), "worker timeout");
}

// Cancellation: dropping the future never leaks the slot.
#[tokio::test(start_paused = true)]
async fn dropping_the_future_during_the_body_does_not_leak_the_slot() {
    let fx = Fixture::new("cancel-body", "60");
    fx.script(
        "/p",
        Behavior::new(Production::NoSignals).tail(Tail::Pending),
    );
    fx.script("/ok", Behavior::new(Production::CleanBeforeEnd));

    let handle = {
        let pool = fx.pool.clone();
        let worker_ref = fx.worker_ref.clone();
        tokio::spawn(async move {
            pool.fetch_worker_completed(&worker_ref, serialized_get("/p"), None, 1024)
                .await
        })
    };
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert!(!handle.is_finished(), "the call is waiting on the body");
    handle.abort();
    let _ = handle.await;

    let next = fx
        .completed("/ok", 1024)
        .await
        .expect("the slot of the cancelled call was released");
    assert_eq!(next.body, Bytes::from_static(b"hello world"));
}

#[tokio::test(start_paused = true)]
async fn dropping_the_future_while_waiting_for_the_release_does_not_leak_the_slot() {
    let fx = Fixture::new("cancel-release", "60");
    fx.script("/n", Behavior::new(Production::NeverSignals));
    fx.script("/ok", Behavior::new(Production::CleanBeforeEnd));

    let handle = {
        let pool = fx.pool.clone();
        let worker_ref = fx.worker_ref.clone();
        tokio::spawn(async move {
            pool.fetch_worker_completed(&worker_ref, serialized_get("/n"), None, 1024)
                .await
        })
    };
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    assert!(!handle.is_finished(), "the call is waiting on the release");
    handle.abort();
    let _ = handle.await;

    // The dispatch state outlives the aborted call only for the pool's own
    // bounded drain wait (2 s + grace), after which the unreleased process is
    // recycled: a bounded hold, not a leak.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let next = fx
        .completed("/ok", 1024)
        .await
        .expect("the slot of the cancelled call was released");
    assert_eq!(next.body, Bytes::from_static(b"hello world"));
    assert_eq!(fx.created(), 2, "the unreleased process was recycled");
    fx.assert_slot_free();
}

// The existing entries keep their behavior: a normal streamed dispatch still
// hands the dispatch state to the body and releases it at the end.
#[tokio::test(start_paused = true)]
async fn existing_stream_entry_is_unchanged() {
    use futures_util::StreamExt;

    let fx = Fixture::new("existing-stream", "60");
    fx.script("/s", Behavior::new(Production::CleanBeforeEnd));

    let response = fx
        .pool
        .fetch_worker_stream(&fx.worker_ref, serialized_get("/s"), None)
        .await
        .expect("stream entry");
    let WorkerResponse::Streamed(mut streamed) = response else {
        panic!("expected a streamed response");
    };
    let mut total = 0;
    while let Some(chunk) = streamed.body.next().await {
        total += chunk.expect("chunk").len();
    }
    assert_eq!(total, 11);
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
    fx.assert_slot_free();
}
