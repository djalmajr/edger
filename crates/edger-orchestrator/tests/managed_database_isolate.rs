#![cfg(unix)]
//! (EDG-54 / 15d) `ManagedDatabaseIsolate`: the asynchronous wrapper that a
//! synchronous factory can build without I/O and that looks the binding up
//! only inside the async `Isolate` operations.
//!
//! Levels of proof (the report keeps them apart):
//!
//! * non-ignored, no runtime: the constructors run in a plain `#[test]` with
//!   no Tokio runtime and touch nothing (ledger, vault); the bridge, the
//!   WASM and the Deno wrappers over the REAL registry/service/vault with a
//!   fake bridge isolate (it records every call and the config it saw) or
//!   with a backend that cannot run anything: a call that REACHES the
//!   backend fails there with that backend's own error
//!   (`WASM_NOT_LOADED`, `UDS_WORKER_DIR`), a call the wrapper DENIES fails
//!   with its static `SQLD_*` error — the two are told apart by the code.
//!   `Ready` here is made by the registry transitions (no SQLD).
//! * `#[ignore]` real runtimes (they FAIL, never skip, when the runtime is
//!   missing): Deno 2.9.8 + SQLD 0.24.32 (`EDGER_DENO_BIN`,
//!   `EDGER_SQLD_BIN`) and the approved Go artifact (`EDGER_GO_WASM_FIXTURE`,
//!   SHA 8062d9cb…). `Ready` there comes ONLY from the service's `ensure`.
//!
//! Not proven here: crashes, multiprocess, SSE through the product, the bin /
//! CLI factory (not wired) and any HTTP mapping of the static errors.
//!
//! ```text
//! EDGER_SQLD_BIN=…/sqld-0.24.32/bin/sqld EDGER_DENO_BIN=…/deno-2.9.8/bin/deno \
//! EDGER_GO_WASM_FIXTURE=…/go-ns-host-root-index.wasm \
//!   cargo test -p edger-orchestrator --test managed_database_isolate \
//!   -- --include-ignored --test-threads=1
//! ```

use std::future::Future;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::{
    parse_worker_config, ExecutionKind, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, StreamedResponse, TerminationOutcome, TerminationReport, WorkerConfig,
    WorkerManifest, WorkerRef, WorkerResponse,
};
use edger_isolation::{DenoProcessIsolate, StreamDetachBudget};
use edger_orchestrator::app_secrets::AppSecretVault;
use edger_orchestrator::managed_database_isolate::ManagedDatabaseIsolate;
use edger_orchestrator::observability::{
    OperationalEvent, OperationalEventQuery, OperationalStore,
};
use edger_orchestrator::sqld_admin::SqldAdminClient;
use edger_orchestrator::sqld_backup::SqldBackupStore;
use edger_orchestrator::sqld_provision::{
    SqldOwner, SqldProvisionService, SqldSystemClock, SQLD_TOKEN_SECRET,
};
use edger_orchestrator::sqld_registry::{
    SqldProvisionRegistry, SqldRegistryOwner, SqldRegistryState, MIN_QUOTA_BYTES,
};
use edger_orchestrator::{EgressGuard, EgressGuardConfig};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::net::TcpListener;
use zeroize::Zeroizing;

const ADMIN_KEY: &str = "canary-admin-managed-isolate";
const LEDGER_MASTER: [u8; 32] = [0x42; 32];
const VAULT_MASTER: [u8; 32] = [0x24; 32];
const QUOTA: u64 = MIN_QUOTA_BYTES;
const BUDGET: u64 = 16 * MIN_QUOTA_BYTES;
const RBAC: &str = "@t";
const ORIGIN: &str = "http://127.0.0.1:8080";
const GO_FIXTURE_SHA: &str = "8062d9cbed8f48dd8f167d7805b2f83e30e401c0d3a072c9d82dc5f810f535b0";
const SCENARIO: Duration = Duration::from_secs(240);

// ───────────────────────── identities / configs ─────────────────────────

fn worker_of(name: &str, namespace: Option<&str>, kind: ExecutionKind) -> WorkerRef {
    let manifest = WorkerManifest {
        name: name.into(),
        entrypoint: Some("index.ts".into()),
        allow_net: Some(vec![]),
        ..WorkerManifest::default()
    };
    WorkerRef {
        id: uuid::Uuid::new_v4(),
        name: name.into(),
        version: "1.0.0".into(),
        dir: PathBuf::from("/nonexistent"),
        namespace: namespace.map(str::to_owned),
        kind,
        config: parse_worker_config(&manifest),
    }
}

fn deno_worker(name: &str) -> WorkerRef {
    worker_of(name, Some(RBAC), ExecutionKind::FetchHandler)
}

fn wasm_worker(name: &str) -> WorkerRef {
    worker_of(name, Some(RBAC), ExecutionKind::WasmModule { entry: None })
}

fn owner_of(worker: &WorkerRef) -> SqldOwner {
    SqldOwner::new(worker.name.clone(), worker.namespace.clone())
}

fn reg_owner(worker: &WorkerRef) -> SqldRegistryOwner<'_> {
    SqldRegistryOwner {
        worker: &worker.name,
        rbac_namespace: worker.namespace.as_deref(),
    }
}

fn request(uri: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: uri.into(),
        headers: vec![],
        body: None,
        request_id: "isolate-test".into(),
        base_href: None,
    }
}

// ───────────────────────── the service (real parts) ─────────────────────────

fn private_dir(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

struct Svc {
    _dir: tempfile::TempDir,
    registry: Arc<SqldProvisionRegistry>,
    vault: Arc<AppSecretVault>,
    service: Arc<SqldProvisionService>,
}

fn build_svc(admin_base: &str, data_base: &str) -> Svc {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    private_dir(dir.path(), "ledger");
    private_dir(dir.path(), "backups");
    let registry = Arc::new(
        SqldProvisionRegistry::open(
            &dir.path().join("ledger").join("ledger.db"),
            Zeroizing::new(LEDGER_MASTER),
            BUDGET,
        )
        .expect("ledger opens"),
    );
    let vault = Arc::new(
        AppSecretVault::open(dir.path().join("vault.db"), Zeroizing::new(VAULT_MASTER))
            .expect("vault opens"),
    );
    let admin = SqldAdminClient::new(admin_base, Zeroizing::new(ADMIN_KEY.to_owned())).unwrap();
    let backup = SqldBackupStore::new(data_base, dir.path().join("backups")).unwrap();
    let service = Arc::new(
        SqldProvisionService::new(
            Arc::clone(&registry),
            Arc::clone(&vault),
            admin,
            backup,
            data_base,
            Arc::new(SqldSystemClock),
        )
        .expect("service builds"),
    );
    Svc {
        _dir: dir,
        registry,
        vault,
        service,
    }
}

fn offline_svc() -> Svc {
    build_svc("http://127.0.0.1:1", "http://127.0.0.1:2")
}

fn store_and_guard() -> (OperationalStore, Arc<EgressGuard>) {
    let store = OperationalStore::default();
    let guard =
        Arc::new(EgressGuard::new(EgressGuardConfig::default()).with_event_store(store.clone()));
    (store, guard)
}

fn egress_events(store: &OperationalStore) -> Vec<OperationalEvent> {
    store
        .query(OperationalEventQuery {
            kind: Some("worker.egress".into()),
            limit: Some(200),
            ..Default::default()
        })
        .events
}

fn vault_token(svc: &Svc, worker: &WorkerRef) -> Option<String> {
    svc.vault
        .get(&worker.name, SQLD_TOKEN_SECRET)
        .map(|v| v.to_string())
}

/// Registry transitions only (no SQLD): the component-level states.
#[derive(Clone, Copy, Debug)]
enum Seed {
    Intent,
    Proven,
    Ready,
    Conflict,
}

fn seed(svc: &Svc, worker: &WorkerRef, state: Seed) {
    let owner = reg_owner(worker);
    svc.registry.ensure_intent(&owner, QUOTA).unwrap();
    match state {
        Seed::Intent => {}
        Seed::Proven | Seed::Ready => {
            let stored = svc.registry.get(&owner).unwrap().unwrap();
            svc.registry.mark_proven(&owner, stored.public_key).unwrap();
            if matches!(state, Seed::Ready) {
                svc.registry.mark_ready(&owner).unwrap();
            }
        }
        Seed::Conflict => {
            svc.registry.mark_conflict(&owner).unwrap();
        }
    }
}

fn state_of(svc: &Svc, worker: &WorkerRef) -> Option<SqldRegistryState> {
    svc.registry
        .get(&reg_owner(worker))
        .unwrap()
        .map(|r| r.state)
}

async fn ensure_ready(svc: &Svc, worker: &WorkerRef) -> String {
    let outcome = svc
        .service
        .ensure(&owner_of(worker), QUOTA)
        .await
        .expect("ensure reaches Ready through the SERVICE (no manual markReady)");
    assert_eq!(outcome.metadata.state, SqldRegistryState::Ready);
    outcome.metadata.sql_namespace
}

async fn within<T>(what: &str, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(SCENARIO, fut)
        .await
        .unwrap_or_else(|_| panic!("{what} exceeded {SCENARIO:?}"))
}

fn code<T: std::fmt::Debug>(result: Result<T, IsolationError>) -> String {
    match result {
        Ok(value) => panic!("expected an error, got Ok({value:?})"),
        Err(error) => error.code,
    }
}

fn assert_static(error: &IsolationError, expected: &str) {
    assert_eq!(error.code, expected);
    let text = format!("{error} {error:?}");
    for forbidden in ["http", "127.0.0.1", "8080", "app", "@t", "eyJ", "Bearer"] {
        assert!(
            !text.contains(forbidden),
            "a static error must not carry `{forbidden}`: {text}"
        );
    }
}

// ───────────────────────── fake bridge isolate ─────────────────────────

#[derive(Default)]
struct Calls {
    names: Mutex<Vec<&'static str>>,
    configs: Mutex<Vec<WorkerConfig>>,
}

impl Calls {
    fn names(&self) -> Vec<&'static str> {
        self.names.lock().unwrap().clone()
    }
    fn record(&self, name: &'static str, config: Option<&WorkerConfig>) {
        self.names.lock().unwrap().push(name);
        if let Some(config) = config {
            self.configs.lock().unwrap().push(config.clone());
        }
    }
}

/// Records every call. `streamed` makes the stream variants return a REAL
/// streamed response; `fail` makes every execution return its own error.
struct FakeIsolate {
    calls: Arc<Calls>,
    /// `execute_fetch` / `execute_routes` leave the host's truncation mark.
    cut: bool,
    streamed: bool,
    fail: bool,
    truncation: bool,
}

fn ok_response(tag: &str) -> SerializedResponse {
    SerializedResponse {
        status: 200,
        headers: vec![("x-fake".into(), tag.into())],
        body: Some(Bytes::from(tag.to_owned())),
    }
}

impl FakeIsolate {
    fn result(&self, tag: &str) -> Result<SerializedResponse, IsolationError> {
        if self.fail {
            Err(IsolationError::new("FAKE_BACKEND", "backend failure"))
        } else {
            Ok(ok_response(tag))
        }
    }
}

#[async_trait]
impl Isolate for FakeIsolate {
    async fn prepare(&mut self, config: &WorkerConfig) -> Result<(), IsolationError> {
        self.calls.record("prepare", Some(config));
        if self.fail {
            return Err(IsolationError::new("FAKE_BACKEND", "backend failure"));
        }
        Ok(())
    }
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.calls.record("fetch", Some(config));
        self.truncation |= self.cut;
        self.result("fetch")
    }
    async fn execute_routes(
        &mut self,
        _req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.calls.record("routes", Some(config));
        self.truncation |= self.cut;
        self.result("routes")
    }
    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.calls.record("spa", Some(config));
        self.result("spa")
    }
    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.calls.record("wasm", Some(config));
        self.result("wasm")
    }
    async fn execute_fetch_stream(
        &mut self,
        _req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.calls.record("fetch_stream", Some(config));
        self.stream("fetch-stream")
    }
    async fn execute_routes_stream(
        &mut self,
        _req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        self.calls.record("routes_stream", Some(config));
        self.stream("routes-stream")
    }
    fn take_response_truncation(&mut self) -> bool {
        std::mem::take(&mut self.truncation)
    }
    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        self.calls.record("notify_idle", None);
        Ok(())
    }
    async fn terminate(&mut self) -> Result<(), IsolationError> {
        self.calls.record("terminate", None);
        Ok(())
    }
    async fn terminate_with_report(&mut self) -> Result<TerminationReport, IsolationError> {
        self.calls.record("terminate_with_report", None);
        Ok(TerminationReport {
            outcome: TerminationOutcome::TimedOut,
            process_id: Some("p-fake".into()),
            drained_count: Some(7),
        })
    }
}

impl FakeIsolate {
    fn stream(&self, tag: &str) -> Result<WorkerResponse, IsolationError> {
        if self.fail {
            return Err(IsolationError::new("FAKE_BACKEND", "backend failure"));
        }
        if !self.streamed {
            return Ok(WorkerResponse::Buffered(ok_response(tag)));
        }
        let chunks: Vec<Result<Bytes, IsolationError>> = vec![
            Ok(Bytes::from(format!("{tag}-1"))),
            Ok(Bytes::from(format!("{tag}-2"))),
        ];
        Ok(WorkerResponse::Streamed(StreamedResponse {
            status: 206,
            headers: vec![("x-fake".into(), tag.into())],
            body: Box::pin(futures_util::stream::iter(chunks)),
            completed: None,
            production_complete: None,
            max_duration_elapsed_ms: None,
            frame_timeout_elapsed_ms: None,
        }))
    }
}

fn fake(streamed: bool, fail: bool) -> (Box<dyn Isolate>, Arc<Calls>) {
    fake_with_cut(streamed, fail, false)
}

fn fake_with_cut(streamed: bool, fail: bool, cut: bool) -> (Box<dyn Isolate>, Arc<Calls>) {
    let calls = Arc::new(Calls::default());
    (
        Box::new(FakeIsolate {
            calls: Arc::clone(&calls),
            cut,
            streamed,
            fail,
            truncation: false,
        }),
        calls,
    )
}

// ═════════════════════ non-ignored: no runtime needed ═════════════════════

/// The three constructors are SYNCHRONOUS pure assembly: this is a plain
/// `#[test]` (no Tokio runtime exists) and nothing is read or written: no
/// ledger record, no vault entry, no ensure. Hostile origins and an empty
/// identity do not panic: they are retained and fail the async operations
/// (see `a_static_assembly_error_fails_every_async_operation`).
#[test]
fn constructors_are_synchronous_and_touch_nothing() {
    assert!(
        tokio::runtime::Handle::try_current().is_err(),
        "no runtime exists in this test"
    );
    let svc = offline_svc();
    let (_, guard) = store_and_guard();
    let worker = deno_worker("ctor-a");
    let wasm = wasm_worker("ctor-a");

    let _deno = ManagedDatabaseIsolate::deno(
        DenoProcessIsolate::new(),
        &worker,
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        ORIGIN,
    );
    let _wasm =
        ManagedDatabaseIsolate::wasm(&wasm, Arc::clone(&svc.service), Arc::clone(&guard), ORIGIN);
    let (inner, calls) = fake(false, false);
    let _bridge = ManagedDatabaseIsolate::bridge(inner, &worker, Arc::clone(&svc.service));
    assert!(calls.names().is_empty(), "the bridge was not touched");

    // Hostile origins / identities: assembled (no panic), no I/O either.
    for bad in [
        "",
        "https://127.0.0.1:8080",
        "http://127.0.0.1:9090",
        "http://u:p@127.0.0.1:8080",
        "http://127.0.0.1:8080/x",
        "not a url",
    ] {
        let _ = ManagedDatabaseIsolate::deno(
            DenoProcessIsolate::new(),
            &worker,
            Arc::clone(&svc.service),
            Arc::clone(&guard),
            bad,
        );
        let _ =
            ManagedDatabaseIsolate::wasm(&wasm, Arc::clone(&svc.service), Arc::clone(&guard), bad);
    }
    let empty = worker_of("", None, ExecutionKind::FetchHandler);
    let _ = ManagedDatabaseIsolate::deno(
        DenoProcessIsolate::new(),
        &empty,
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        ORIGIN,
    );

    assert!(state_of(&svc, &worker).is_none(), "no ledger record");
    assert!(vault_token(&svc, &worker).is_none(), "no vault entry");
}

/// A static assembly error (invalid origin / empty identity) fails every
/// async backend operation with a static error, never delegates, and the
/// service creates nothing. WASM has no isolate at all in that shape.
/// The host's truncation mark (EDG-55) lives in the retained backend: the
/// wrapper in front of it must forward `take_response_truncation`, or a
/// managed-database `ttl: 0` worker would lose it and fail open. The default
/// of the trait is `false`, so only a forwarded `true` proves the delegation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_forwards_the_hosts_truncation_mark_and_reading_clears_it() {
    let svc = offline_svc();
    let worker = deno_worker("br-cut");
    let config = worker.config.clone();

    // Marked backend: nothing before a call, the mark after an execution, one
    // read clears it, for both buffered entries.
    for routes in [false, true] {
        let (inner, _calls) = fake_with_cut(false, false, true);
        let mut bridge = ManagedDatabaseIsolate::bridge(inner, &worker, Arc::clone(&svc.service));
        assert!(!bridge.take_response_truncation(), "no mark before a call");
        if routes {
            bridge.execute_routes(request("/"), &config).await.unwrap();
        } else {
            bridge.execute_fetch(request("/"), &config).await.unwrap();
        }
        assert!(
            bridge.take_response_truncation(),
            "forwarded (routes={routes})"
        );
        assert!(!bridge.take_response_truncation(), "reading clears it");
    }

    // Control: the same backend without a mark.
    let (inner, calls) = fake_with_cut(false, false, false);
    let mut bridge = ManagedDatabaseIsolate::bridge(inner, &worker, Arc::clone(&svc.service));
    bridge.execute_fetch(request("/"), &config).await.unwrap();
    assert!(!bridge.take_response_truncation());
    assert_eq!(calls.names(), vec!["fetch"]);
}

/// A wrapper whose backend is unavailable (failed static assembly: WASM with
/// no isolate) executed nothing, so it has no mark to give.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unavailable_backend_has_no_truncation_mark() {
    let svc = offline_svc();
    let (_, guard) = store_and_guard();
    let wasm = wasm_worker("cut-unavailable");
    let mut isolate = ManagedDatabaseIsolate::wasm(
        &wasm,
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        "http://127.0.0.1:9090",
    );
    assert!(isolate
        .execute_wasm(request("/"), &wasm.config)
        .await
        .is_err());
    assert!(!isolate.take_response_truncation());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_static_assembly_error_fails_every_async_operation() {
    let svc = offline_svc();
    let (_, guard) = store_and_guard();
    let wasm = wasm_worker("static-w");
    let deno = deno_worker("static-d");
    // Even a Ready binding must not rescue a failed assembly.
    seed(&svc, &wasm, Seed::Ready);
    seed(&svc, &deno, Seed::Ready);

    let mut isolates: Vec<(&str, ManagedDatabaseIsolate, WorkerConfig)> = vec![
        (
            "wasm bad origin",
            ManagedDatabaseIsolate::wasm(
                &wasm,
                Arc::clone(&svc.service),
                Arc::clone(&guard),
                "http://127.0.0.1:9090",
            ),
            wasm.config.clone(),
        ),
        (
            "deno bad origin",
            ManagedDatabaseIsolate::deno(
                DenoProcessIsolate::new(),
                &deno,
                Arc::clone(&svc.service),
                Arc::clone(&guard),
                "https://example.com",
            ),
            deno.config.clone(),
        ),
        (
            "deno empty identity",
            ManagedDatabaseIsolate::deno(
                DenoProcessIsolate::new(),
                &worker_of("", None, ExecutionKind::FetchHandler),
                Arc::clone(&svc.service),
                Arc::clone(&guard),
                ORIGIN,
            ),
            deno.config.clone(),
        ),
    ];
    for (label, isolate, config) in &mut isolates {
        let r = isolate.prepare(config).await.unwrap_err();
        assert_static(&r, "SQLD_BINDING_UNAVAILABLE");
        for result in [
            isolate
                .execute_fetch(request("/"), config)
                .await
                .map(|_| ()),
            isolate
                .execute_routes(request("/"), config)
                .await
                .map(|_| ()),
            isolate.execute_wasm(request("/"), config).await.map(|_| ()),
            isolate
                .execute_fetch_stream(request("/"), config)
                .await
                .map(|_| ()),
            isolate
                .execute_routes_stream(request("/"), config)
                .await
                .map(|_| ()),
        ] {
            assert_static(&result.unwrap_err(), "SQLD_BINDING_UNAVAILABLE");
        }
        eprintln!("static-assembly: {label} denied");
    }
    // The WASM shape has no backend at all: even the lifecycle reports it.
    let (_, wasm_isolate, _) = &mut isolates[0];
    assert_static(
        &wasm_isolate.terminate_with_report().await.unwrap_err(),
        "SQLD_BINDING_UNAVAILABLE",
    );
    assert_static(
        &wasm_isolate
            .serve_static_spa("/", None, &wasm.config)
            .await
            .unwrap_err(),
        "SQLD_BINDING_UNAVAILABLE",
    );
    // Nothing was created by the denials.
    assert_eq!(state_of(&svc, &wasm), Some(SqldRegistryState::Ready));
    assert!(vault_token(&svc, &wasm).is_none());
    assert!(vault_token(&svc, &deno).is_none());
}

/// Bridge CONTROL: an owner without any record delegates the legacy
/// behavior exactly, method by method (config untouched, errors, report and
/// the streaming variant preserved).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_delegates_the_legacy_behavior_when_the_owner_has_no_record() {
    let svc = offline_svc();
    let worker = deno_worker("br-absent");
    assert!(state_of(&svc, &worker).is_none());
    let (inner, calls) = fake(true, false);
    let mut bridge = ManagedDatabaseIsolate::bridge(inner, &worker, Arc::clone(&svc.service));
    let mut config = worker.config.clone();
    config.env.insert("MARK".into(), "current".into());

    bridge.prepare(&config).await.unwrap();
    assert_eq!(
        bridge.execute_fetch(request("/"), &config).await.unwrap(),
        ok_response("fetch")
    );
    assert_eq!(
        bridge.execute_routes(request("/"), &config).await.unwrap(),
        ok_response("routes")
    );
    assert_eq!(
        bridge.execute_wasm(request("/"), &config).await.unwrap(),
        ok_response("wasm")
    );
    assert_eq!(
        bridge
            .serve_static_spa("/p", Some("/b"), &config)
            .await
            .unwrap(),
        ok_response("spa")
    );
    // Streaming variants keep the STREAMED variant (not buffered).
    for (label, response) in [
        (
            "fetch",
            bridge
                .execute_fetch_stream(request("/"), &config)
                .await
                .unwrap(),
        ),
        (
            "routes",
            bridge
                .execute_routes_stream(request("/"), &config)
                .await
                .unwrap(),
        ),
    ] {
        let WorkerResponse::Streamed(mut streamed) = response else {
            panic!("{label}: the streaming variant must survive the wrapper");
        };
        assert_eq!(streamed.status, 206);
        let first = streamed.body.next().await.unwrap().unwrap();
        assert!(first.ends_with(b"-1"), "{label}");
        let second = streamed.body.next().await.unwrap().unwrap();
        assert!(second.ends_with(b"-2"), "{label}");
        assert!(streamed.body.next().await.is_none());
    }
    bridge.notify_idle().await.unwrap();
    bridge.terminate().await.unwrap();
    let report = bridge.terminate_with_report().await.unwrap();
    assert_eq!(
        report,
        TerminationReport {
            outcome: TerminationOutcome::TimedOut,
            process_id: Some("p-fake".into()),
            drained_count: Some(7),
        },
        "the report is the inner one, untouched"
    );
    assert_eq!(
        calls.names(),
        [
            "prepare",
            "fetch",
            "routes",
            "wasm",
            "spa",
            "fetch_stream",
            "routes_stream",
            "notify_idle",
            "terminate",
            "terminate_with_report"
        ]
    );
    // Every config that crossed is the CURRENT one, unchanged (no SQLD).
    for seen in calls.configs.lock().unwrap().iter() {
        assert_eq!(seen, &config);
    }
    assert!(state_of(&svc, &worker).is_none(), "nothing was created");

    // Inner errors are preserved as they are.
    let (inner, _) = fake(false, true);
    let mut failing = ManagedDatabaseIsolate::bridge(inner, &worker, Arc::clone(&svc.service));
    assert_eq!(
        code(failing.execute_fetch(request("/"), &config).await),
        "FAKE_BACKEND"
    );
    assert_eq!(code(failing.prepare(&config).await), "FAKE_BACKEND");
    assert_eq!(
        code(failing.execute_fetch_stream(request("/"), &config).await),
        "FAKE_BACKEND"
    );
}

/// Bridge NEGATIVE: ANY registry record refuses every backend operation
/// (the fake sees none), while static/lifecycle operations still delegate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_refuses_backend_execution_for_every_registry_record() {
    for state in [Seed::Intent, Seed::Proven, Seed::Ready, Seed::Conflict] {
        let svc = offline_svc();
        let worker = deno_worker("br-record");
        seed(&svc, &worker, state);
        let (inner, calls) = fake(true, false);
        let mut bridge = ManagedDatabaseIsolate::bridge(inner, &worker, Arc::clone(&svc.service));
        let config = worker.config.clone();

        let denied = [
            bridge.prepare(&config).await.unwrap_err(),
            bridge
                .execute_fetch(request("/"), &config)
                .await
                .unwrap_err(),
            bridge
                .execute_routes(request("/"), &config)
                .await
                .unwrap_err(),
            bridge
                .execute_wasm(request("/"), &config)
                .await
                .unwrap_err(),
            bridge
                .execute_fetch_stream(request("/"), &config)
                .await
                .unwrap_err(),
            bridge
                .execute_routes_stream(request("/"), &config)
                .await
                .unwrap_err(),
        ];
        for error in &denied {
            assert_static(error, "SQLD_MANAGED_RUNTIME_UNSUPPORTED");
        }
        assert!(
            calls.names().is_empty(),
            "{state:?}: the legacy backend must not run: {:?}",
            calls.names()
        );
        // Not a backend execution: these keep delegating.
        bridge
            .serve_static_spa("/", None, &config)
            .await
            .expect("spa delegates");
        bridge.notify_idle().await.unwrap();
        bridge.terminate().await.unwrap();
        assert_eq!(
            bridge.terminate_with_report().await.unwrap().process_id,
            Some("p-fake".into())
        );
        assert_eq!(
            calls.names(),
            ["spa", "notify_idle", "terminate", "terminate_with_report"],
            "{state:?}"
        );
        // Nothing was created or changed.
        let expected = match state {
            Seed::Intent => SqldRegistryState::Intent,
            Seed::Proven => SqldRegistryState::Proven,
            Seed::Ready => SqldRegistryState::Ready,
            Seed::Conflict => SqldRegistryState::Conflict,
        };
        assert_eq!(state_of(&svc, &worker), Some(expected));
        assert!(vault_token(&svc, &worker).is_none());
    }
}

/// A lookup ERROR (a namespace the ledger rejects) fails closed in the
/// three shapes with `SQLD_BINDING_UNAVAILABLE`, running nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lookup_error_fails_closed_in_every_shape() {
    let svc = offline_svc();
    let (_, guard) = store_and_guard();
    let bad_ns = "bad\u{1}ns";
    let deno = worker_of("lk-d", Some(bad_ns), ExecutionKind::FetchHandler);
    let wasm = worker_of(
        "lk-w",
        Some(bad_ns),
        ExecutionKind::WasmModule { entry: None },
    );
    assert!(
        svc.service.metadata(&owner_of(&deno)).await.is_err(),
        "precondition: the lookup itself errors"
    );

    let (inner, calls) = fake(false, false);
    let mut bridge = ManagedDatabaseIsolate::bridge(inner, &deno, Arc::clone(&svc.service));
    let mut dn = ManagedDatabaseIsolate::deno(
        DenoProcessIsolate::new(),
        &deno,
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        ORIGIN,
    );
    let mut wm =
        ManagedDatabaseIsolate::wasm(&wasm, Arc::clone(&svc.service), Arc::clone(&guard), ORIGIN);
    for (label, isolate, config) in [
        ("bridge", &mut bridge, &deno.config),
        ("deno", &mut dn, &deno.config),
        ("wasm", &mut wm, &wasm.config),
    ] {
        assert_static(
            &isolate
                .execute_fetch(request("/"), config)
                .await
                .unwrap_err(),
            "SQLD_BINDING_UNAVAILABLE",
        );
        assert_static(
            &isolate
                .execute_wasm(request("/"), config)
                .await
                .unwrap_err(),
            "SQLD_BINDING_UNAVAILABLE",
        );
        assert_static(
            &isolate.prepare(config).await.unwrap_err(),
            "SQLD_BINDING_UNAVAILABLE",
        );
        eprintln!("lookup-error: {label} denied");
    }
    assert!(calls.names().is_empty());
    // The Deno shape never started a process.
    let report = dn.terminate_with_report().await.unwrap();
    assert_eq!(report.outcome, TerminationOutcome::NotRunning);
}

/// Deno WITHOUT a process: Intent/Proven/Conflict deny with
/// `SQLD_BINDING_NOT_READY`; a Ready binding with a conflicting CURRENT
/// config denies with `SQLD_CONFIG_CONFLICT` and the origin config is
/// untouched; either way no process exists afterwards. The CONTROL (Ready +
/// clean current config) passes the gate and reaches the backend, which
/// fails with its own `UDS_WORKER_DIR` (no worker dir; nothing is spawned).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deno_gate_denies_partial_states_and_conflicts_and_passes_the_control() {
    let (_, guard) = store_and_guard();
    let build = |svc: &Svc, worker: &WorkerRef| {
        ManagedDatabaseIsolate::deno(
            DenoProcessIsolate::new(),
            worker,
            Arc::clone(&svc.service),
            Arc::clone(&guard),
            ORIGIN,
        )
    };

    for state in [Seed::Intent, Seed::Proven, Seed::Conflict] {
        let svc = offline_svc();
        let worker = deno_worker("dn-state");
        seed(&svc, &worker, state);
        let mut isolate = build(&svc, &worker);
        for result in [
            isolate.prepare(&worker.config).await,
            isolate
                .execute_fetch(request("/"), &worker.config)
                .await
                .map(|_| ()),
            isolate
                .execute_fetch_stream(request("/"), &worker.config)
                .await
                .map(|_| ()),
        ] {
            assert_static(&result.unwrap_err(), "SQLD_BINDING_NOT_READY");
        }
        assert_eq!(
            isolate.terminate_with_report().await.unwrap().outcome,
            TerminationOutcome::NotRunning,
            "{state:?}: no process"
        );
    }

    // Ready: conflicts come from the CURRENT config (the constructor's is
    // clean), each one denies and leaves the config as it was.
    let svc = offline_svc();
    let worker = deno_worker("dn-ready");
    seed(&svc, &worker, Seed::Ready);
    let mut isolate = build(&svc, &worker);
    type Mutation = Box<dyn Fn(&mut WorkerConfig)>;
    let current_variants: Vec<(&str, Mutation)> = vec![
        (
            "SQLD_URL elsewhere",
            Box::new(|c| {
                c.env
                    .insert("SQLD_URL".into(), "http://10.9.9.9:8080".into());
            }),
        ),
        (
            "SQLD_TOKEN env",
            Box::new(|c| {
                c.env.insert("SQLD_TOKEN".into(), "x".into());
            }),
        ),
        (
            "SQLD_TOKEN publicEnv",
            Box::new(|c| c.public_env.push(" SQLD_TOKEN ".into())),
        ),
        (
            "SQLD_URL secret",
            Box::new(|c| c.secrets.push("SQLD_URL".into())),
        ),
        ("allowNet None", Box::new(|c| c.allow_net = None)),
        (
            "allowNet non-empty",
            Box::new(|c| c.allow_net = Some(vec!["example.com".into()])),
        ),
    ];
    for (label, mutate) in current_variants {
        let mut current = worker.config.clone();
        mutate(&mut current);
        let before = current.clone();
        let error = isolate
            .execute_fetch(request("/"), &current)
            .await
            .unwrap_err();
        assert_static(&error, "SQLD_CONFIG_CONFLICT");
        assert_eq!(current, before, "{label}: the config is never overwritten");
        assert_eq!(
            isolate.terminate_with_report().await.unwrap().outcome,
            TerminationOutcome::NotRunning,
            "{label}: no process"
        );
    }

    // CONTROL (current config clean, Ready): the gate passes and the
    // BACKEND answers (UDS_WORKER_DIR is the Deno backend's own error).
    let error = isolate
        .execute_fetch(request("/"), &worker.config)
        .await
        .unwrap_err();
    assert_eq!(
        error.code, "UDS_WORKER_DIR",
        "the control reached the backend"
    );

    // The constructor's config is NOT reused: a constructor config full of
    // conflicts (stale SQLD_TOKEN env, foreign SQLD_URL) is irrelevant when
    // the pool's current config is clean — the control passes again.
    let mut stale = deno_worker("dn-ready");
    stale.config.env.insert("SQLD_TOKEN".into(), "stale".into());
    stale
        .config
        .env
        .insert("SQLD_URL".into(), "http://10.9.9.9:8080".into());
    let mut from_stale = build(&svc, &stale);
    let error = from_stale
        .execute_fetch(request("/"), &worker.config)
        .await
        .unwrap_err();
    assert_eq!(
        error.code, "UDS_WORKER_DIR",
        "the clean CURRENT config governs, not the constructor's"
    );
    assert_eq!(
        isolate.terminate_with_report().await.unwrap().outcome,
        TerminationOutcome::NotRunning
    );
}

/// Deno WITHOUT binding: the original config reaches the backend (control
/// `UDS_WORKER_DIR`) even when it carries values that WOULD conflict with a
/// managed binding (the helper is not applied without a record).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deno_without_a_record_keeps_the_original_config() {
    let svc = offline_svc();
    let (_, guard) = store_and_guard();
    let worker = deno_worker("dn-absent");
    let mut isolate = ManagedDatabaseIsolate::deno(
        DenoProcessIsolate::new(),
        &worker,
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        ORIGIN,
    );
    let mut config = worker.config.clone();
    config.env.insert("SQLD_TOKEN".into(), "legacy".into());
    config.allow_net = None;
    let error = isolate
        .execute_fetch(request("/"), &config)
        .await
        .unwrap_err();
    assert_eq!(error.code, "UDS_WORKER_DIR", "absent: legacy, no denial");
    assert!(state_of(&svc, &worker).is_none(), "nothing was created");
    assert_eq!(
        isolate.terminate_with_report().await.unwrap().outcome,
        TerminationOutcome::NotRunning
    );
    // serve_static_spa never consults the binding: it is the inner's own.
    let inner_direct = DenoProcessIsolate::new()
        .serve_static_spa("/", None, &config)
        .await
        .map_err(|e| e.code);
    let wrapped = isolate
        .serve_static_spa("/", None, &config)
        .await
        .map_err(|e| e.code);
    assert_eq!(wrapped, inner_direct);
}

/// WASM: absent/Ready reach the retained backend (its own
/// `WASM_NOT_LOADED`: no module bytes in this fixture), partial states and
/// conflicts deny; the CURRENT config governs; backend errors and the
/// lifecycle delegate exactly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wasm_gate_reaches_the_backend_only_for_absent_or_ready() {
    let (_, guard) = store_and_guard();
    let build = |svc: &Svc, worker: &WorkerRef| {
        ManagedDatabaseIsolate::wasm(worker, Arc::clone(&svc.service), Arc::clone(&guard), ORIGIN)
    };

    // Absent: original config, the backend answers (even with a config that
    // would conflict for a Ready binding).
    let svc = offline_svc();
    let worker = wasm_worker("wm-absent");
    let mut isolate = build(&svc, &worker);
    let mut conflicting = worker.config.clone();
    conflicting.env.insert("SQLD_TOKEN".into(), "legacy".into());
    assert_eq!(
        code(isolate.execute_wasm(request("/"), &conflicting).await),
        "WASM_NOT_LOADED"
    );
    assert!(state_of(&svc, &worker).is_none(), "nothing was created");

    for state in [Seed::Intent, Seed::Proven, Seed::Conflict] {
        let svc = offline_svc();
        let worker = wasm_worker("wm-state");
        seed(&svc, &worker, state);
        let mut isolate = build(&svc, &worker);
        for result in [
            isolate.prepare(&worker.config).await,
            isolate
                .execute_wasm(request("/"), &worker.config)
                .await
                .map(|_| ()),
            isolate
                .execute_fetch_stream(request("/"), &worker.config)
                .await
                .map(|_| ()),
        ] {
            assert_static(&result.unwrap_err(), "SQLD_BINDING_NOT_READY");
        }
    }

    // Ready: the CURRENT config decides (conflict denies, clean reaches the
    // backend); the constructor config is irrelevant in both directions.
    let svc = offline_svc();
    let worker = wasm_worker("wm-ready");
    seed(&svc, &worker, Seed::Ready);
    let mut isolate = build(&svc, &worker);
    let mut bad = worker.config.clone();
    bad.public_env.push("SQLD_TOKEN".into());
    let before = bad.clone();
    assert_static(
        &isolate.execute_wasm(request("/"), &bad).await.unwrap_err(),
        "SQLD_CONFIG_CONFLICT",
    );
    assert_eq!(bad, before, "the current config is not mutated");
    let mut url_conflict = worker.config.clone();
    url_conflict
        .env
        .insert("SQLD_URL".into(), "http://10.9.9.9:8080".into());
    assert_static(
        &isolate
            .execute_wasm(request("/"), &url_conflict)
            .await
            .unwrap_err(),
        "SQLD_CONFIG_CONFLICT",
    );
    assert_eq!(
        code(isolate.execute_wasm(request("/"), &worker.config).await),
        "WASM_NOT_LOADED",
        "clean current config passes the gate"
    );
    let mut stale = wasm_worker("wm-ready");
    stale.config.env.insert("SQLD_TOKEN".into(), "stale".into());
    let mut from_stale = build(&svc, &stale);
    assert_eq!(
        code(from_stale.execute_wasm(request("/"), &worker.config).await),
        "WASM_NOT_LOADED",
        "a conflicting CONSTRUCTOR config does not poison a clean current one"
    );
    let mut clean_ctor = build(&svc, &worker);
    assert_static(
        &clean_ctor
            .execute_wasm(request("/"), &bad)
            .await
            .unwrap_err(),
        "SQLD_CONFIG_CONFLICT",
    );

    // Backend errors and lifecycle delegate exactly (the WasmIsolate's own).
    assert_eq!(
        code(isolate.execute_fetch(request("/"), &worker.config).await),
        "NOT_IMPLEMENTED"
    );
    assert_eq!(
        code(isolate.execute_routes(request("/"), &worker.config).await),
        "NOT_IMPLEMENTED"
    );
    assert_eq!(
        code(
            isolate
                .execute_routes_stream(request("/"), &worker.config)
                .await
        ),
        "NOT_IMPLEMENTED",
        "the streaming entry delegates (and keeps the inner error)"
    );
    assert_eq!(
        code(isolate.serve_static_spa("/", None, &worker.config).await),
        "NOT_IMPLEMENTED"
    );
    assert_eq!(code(isolate.notify_idle().await), "NOT_IMPLEMENTED");
    assert_eq!(code(isolate.terminate().await), "NOT_IMPLEMENTED");
    assert_eq!(
        code(isolate.terminate_with_report().await),
        "NOT_IMPLEMENTED"
    );

    // A lookup error is not hidden by a Ready sibling: handled in
    // `a_lookup_error_fails_closed_in_every_shape`.
}

const ENV_DUMP_WAT: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "environ_sizes_get"
        (func $sizes (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "environ_get"
        (func $get (param i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 512) "[[\"content-type\",\"text/plain\"]]")
      (global $heap (mut i32) (i32.const 49000))

      (func (export "edger_alloc") (param $len i32) (result i32)
        (local $ptr i32)
        global.get $heap
        local.set $ptr
        global.get $heap
        local.get $len
        i32.add
        global.set $heap
        local.get $ptr
      )

      (func $copy (param $dst i32) (param $src i32) (param $len i32)
        (local $i i32)
        loop $copy_loop
          local.get $i
          local.get $len
          i32.lt_u
          if
            local.get $dst
            local.get $i
            i32.add
            local.get $src
            local.get $i
            i32.add
            i32.load8_u
            i32.store8
            local.get $i
            i32.const 1
            i32.add
            local.set $i
            br $copy_loop
          end
        end
      )

      (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
        (local $status i32)
        (local $count i32)
        (local $buf_size i32)
        (local $body_len i32)
        (local $frame_len i32)

        ;; environ_sizes_get(count_ptr=1000, size_ptr=1004) -> errno
        i32.const 1000
        i32.const 1004
        call $sizes
        local.set $status
        local.get $status
        i32.eqz
        if
        else
          unreachable
        end

        i32.const 1000
        i32.load
        local.set $count
        i32.const 1004
        i32.load
        local.set $buf_size

        ;; Guard: the deterministic test env fits the 8 KiB scratch
        ;; buffer @ 8000 (memory is one 64 KiB page).
        local.get $buf_size
        i32.const 8192
        i32.gt_s
        if
          unreachable
        end

        ;; environ_get(env_ptrs=2000, env_buf=8000) -> errno
        i32.const 2000
        i32.const 8000
        call $get
        local.set $status
        local.get $status
        i32.eqz
        if
        else
          unreachable
        end

        ;; Response frame @ 40000 (12-byte header):
        ;;   u16 status = 200 @ 40000
        ;;   u32 headers_len = 31 @ 40004
        ;;   u32 body_len = 8 + buf_size @ 40008
        i32.const 40000
        i32.const 200
        i32.store16
        i32.const 40004
        i32.const 31
        i32.store
        local.get $buf_size
        i32.const 8
        i32.add
        local.set $body_len
        i32.const 40008
        local.get $body_len
        i32.store

        ;; Headers (the 31-byte JSON) @ 40012; body @ 40043:
        ;; [u32 count][u32 buf_size][entries from 8000]
        i32.const 40012
        i32.const 512
        i32.const 31
        call $copy

        i32.const 40043
        local.get $count
        i32.store
        i32.const 40047
        local.get $buf_size
        i32.store
        i32.const 40051
        i32.const 8000
        local.get $buf_size
        call $copy

        i32.const 40000
        i64.extend_i32_u
        i32.const 12
        local.get $body_len
        i32.add
        i32.const 31
        i32.add
        local.set $frame_len
        local.get $frame_len
        i64.extend_i32_u
        i64.const 32
        i64.shl
        i64.or
      )
    )
"#;

/// Walks the guest dump the way a guest `getenv` would: NUL-terminated
/// `NAME=VALUE` entries in buffer order (the FIRST entry of a name wins).
fn parse_env_body(body: &[u8]) -> Vec<(String, String)> {
    assert!(body.len() >= 8, "body too short: {body:?}");
    let buf_size = u32::from_le_bytes(body[4..8].try_into().unwrap()) as usize;
    assert!(
        body.len() >= 8 + buf_size,
        "body shorter than the env buffer"
    );
    body[8..8 + buf_size]
        .split(|&b| b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let entry = String::from_utf8(entry.to_vec()).unwrap();
            let (name, value) = entry.split_once('=').expect("NAME=VALUE");
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

fn env_value(env: &[(String, String)], name: &str) -> Option<String> {
    env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

/// A WASM worker whose module is the env-dump WAT (loaded by the isolate from
/// the worker dir). `env` is the config env the CALLER passes.
struct EnvFx {
    dir: tempfile::TempDir,
    worker: WorkerRef,
}

fn env_fx(name: &str, env: &[(&str, &str)]) -> EnvFx {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.wat"), ENV_DUMP_WAT).unwrap();
    let mut worker = wasm_worker(name);
    worker.config.worker_dir = Some(dir.path().to_path_buf());
    worker.config.entrypoint = Some("index.wat".into());
    worker.config.timeout_ms = 30_000;
    worker.config.env = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    EnvFx { dir, worker }
}

fn config_env(base: &WorkerConfig, env: &[(&str, &str)]) -> WorkerConfig {
    let mut config = base.clone();
    config.env = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    config
}

/// The env the guest REALLY sees (the WASI `environ_get` of that execution).
async fn guest_env(
    isolate: &mut ManagedDatabaseIsolate,
    config: &WorkerConfig,
) -> Result<Vec<(String, String)>, IsolationError> {
    let response = isolate.execute_wasm(request("/"), config).await?;
    assert_eq!(response.status, 200);
    Ok(parse_env_body(response.body.as_deref().unwrap()))
}

fn env_wasm_wrapper(svc: &Svc, worker: &WorkerRef) -> ManagedDatabaseIsolate {
    let (_, guard) = store_and_guard();
    ManagedDatabaseIsolate::wasm(worker, Arc::clone(&svc.service), guard, ORIGIN)
}

/// No record: the guest sees the CURRENT call's env (executed WASI
/// `environ_get`), not the constructor's, and a variable removed by a later
/// call does not come back with its old value; one isolate/bytes serve all
/// calls (the module file is removed after the first).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wasm_guest_sees_the_current_call_env_without_a_record() {
    let svc = offline_svc();
    let fx = env_fx("wm-env-a", &[("VALUE", "old")]);
    let mut isolate = env_wasm_wrapper(&svc, &fx.worker);

    let env = guest_env(
        &mut isolate,
        &config_env(&fx.worker.config, &[("VALUE", "new")]),
    )
    .await
    .unwrap();
    assert_eq!(env_value(&env, "VALUE").as_deref(), Some("new"), "{env:?}");
    std::fs::remove_file(fx.dir.path().join("index.wat")).unwrap();
    let env = guest_env(
        &mut isolate,
        &config_env(&fx.worker.config, &[("OTHER", "x")]),
    )
    .await
    .expect("second call runs from the retained bytes");
    assert_eq!(
        env_value(&env, "VALUE"),
        None,
        "old/new value must not return: {env:?}"
    );
    assert_eq!(env_value(&env, "OTHER").as_deref(), Some("x"));
    let env = guest_env(&mut isolate, &config_env(&fx.worker.config, &[]))
        .await
        .unwrap();
    assert!(env.is_empty(), "no env at all: {env:?}");
    assert!(state_of(&svc, &fx.worker).is_none(), "nothing was created");
}

/// No record: an empty/absent `SQLD_URL` stays exactly as the caller gave it
/// (nothing is anticipated into the constructor's env, no host origin).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wasm_without_a_record_does_not_anticipate_sqld_url() {
    let svc = offline_svc();
    let fx = env_fx("wm-env-b", &[("SQLD_URL", "")]);
    let mut isolate = env_wasm_wrapper(&svc, &fx.worker);
    let env = guest_env(
        &mut isolate,
        &config_env(&fx.worker.config, &[("SQLD_URL", "")]),
    )
    .await
    .unwrap();
    assert_eq!(env_value(&env, "SQLD_URL").as_deref(), Some(""), "{env:?}");
    let env = guest_env(&mut isolate, &config_env(&fx.worker.config, &[]))
        .await
        .unwrap();
    assert_eq!(env_value(&env, "SQLD_URL"), None, "{env:?}");
    let env = guest_env(&mut isolate, &config_env(&fx.worker.config, &[("A", "1")]))
        .await
        .unwrap();
    assert!(!format!("{env:?}").contains("127.0.0.1"), "{env:?}");
}

/// Ready: the CURRENT config passes through the helper clone, so the guest
/// sees the host origin even when the CONSTRUCTOR config conflicts; a
/// conflicting CURRENT config denies without executing the guest; the
/// config passed in is never mutated.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wasm_ready_guest_sees_the_host_origin_from_the_current_clone() {
    let svc = offline_svc();
    let fx = env_fx("wm-env-c", &[("SQLD_URL", "http://10.9.9.9:8080")]);
    seed(&svc, &fx.worker, Seed::Ready);
    let mut isolate = env_wasm_wrapper(&svc, &fx.worker);

    let clean = config_env(&fx.worker.config, &[("VALUE", "v1")]);
    let before = clean.clone();
    let env = guest_env(&mut isolate, &clean).await.unwrap();
    assert_eq!(
        env_value(&env, "SQLD_URL").as_deref(),
        Some("http://127.0.0.1:8080/"),
        "host origin from the current clone: {env:?}"
    );
    assert_eq!(env_value(&env, "VALUE").as_deref(), Some("v1"));
    assert_eq!(clean, before, "the passed config is not mutated");

    let conflict = config_env(
        &fx.worker.config,
        &[("SQLD_URL", "http://10.9.9.9:8080"), ("VALUE", "v2")],
    );
    let error = guest_env(&mut isolate, &conflict).await.unwrap_err();
    assert_static(&error, "SQLD_CONFIG_CONFLICT");
    // A denial leaves the isolate usable and the NEXT clean call sees only
    // its own env (the conflicting call's VALUE=v2 never reached the guest).
    let env = guest_env(
        &mut isolate,
        &config_env(&fx.worker.config, &[("VALUE", "v3")]),
    )
    .await
    .unwrap();
    assert_eq!(env_value(&env, "VALUE").as_deref(), Some("v3"));
    assert_eq!(
        env_value(&env, "SQLD_URL").as_deref(),
        Some("http://127.0.0.1:8080/")
    );
}

// ═════════════════════ real SQLD ═════════════════════

struct Sqld {
    child: Option<tokio::process::Child>,
    pid: u32,
    user_base: String,
    admin_base: String,
}

fn require_env_file(var: &str) -> String {
    let value = std::env::var(var)
        .unwrap_or_else(|_| panic!("{var} must name the real runtime; this test never skips"));
    assert!(Path::new(&value).is_file(), "{var} does not name a file");
    value
}

/// Two DISTINCT ephemeral loopback ports for one process. Both probe
/// listeners stay open until both ports are known and only then are released:
/// two calls to a one-port helper could get the same port back from the
/// kernel, because the first probe is already closed when the second binds.
async fn ephemeral_port_pair() -> (u16, u16) {
    let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let second = TcpListener::bind("127.0.0.1:0").await.unwrap();
    (
        first.local_addr().unwrap().port(),
        second.local_addr().unwrap().port(),
    )
}

fn direct_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap()
}

impl Sqld {
    async fn start(dir: &Path) -> Self {
        let bin = require_env_file("EDGER_SQLD_BIN");
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("ed25519 pair");
        let pub_path = dir.join("global.pub");
        std::fs::write(&pub_path, pair.public_key_pem()).unwrap();
        let (user_port, admin_port) = ephemeral_port_pair().await;
        assert_ne!(user_port, admin_port, "the two fixture ports are distinct");
        let child = tokio::process::Command::new(bin)
            .arg("--db-path")
            .arg(dir.join("data.sqld"))
            .arg("--http-listen-addr")
            .arg(format!("127.0.0.1:{user_port}"))
            .arg("--admin-listen-addr")
            .arg(format!("127.0.0.1:{admin_port}"))
            .arg("--admin-auth-key")
            .arg(ADMIN_KEY)
            .arg("--enable-namespaces")
            .arg("--disable-default-namespace")
            .arg("--auth-jwt-key-file")
            .arg(&pub_path)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("RUST_LOG", "error")
            .stdout(std::fs::File::create(dir.join("sqld.out")).unwrap())
            .stderr(std::fs::File::create(dir.join("sqld.err")).unwrap())
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|e| panic!("cannot spawn the real sqld: {e}"));
        let pid = child.id().expect("child pid");
        let mut sqld = Self {
            child: Some(child),
            pid,
            user_base: format!("http://127.0.0.1:{user_port}"),
            admin_base: format!("http://127.0.0.1:{admin_port}"),
        };
        let client = direct_client();
        let url = format!("{}/v1/namespaces/edger_probe/config", sqld.admin_base);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(Ok(Some(status))) = sqld.child.as_mut().map(|c| c.try_wait()) {
                panic!("sqld exited during startup: {status:?}");
            }
            match client
                .get(&url)
                .header("authorization", format!("Basic {ADMIN_KEY}"))
                .send()
                .await
            {
                Ok(r) if r.status().as_u16() == 404 => break,
                Ok(r) => panic!("admin gate answered {} before ready", r.status()),
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(200)).await
                }
                Err(_) => panic!("the admin gate did not answer in time"),
            }
        }
        sqld
    }

    /// Kill + reap the OWNED child and prove its pid is gone.
    async fn shutdown(&mut self) -> String {
        let Some(mut child) = self.child.take() else {
            return "already stopped".into();
        };
        let kill = child.kill().await;
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("the owned sqld child is reaped within 10 s")
            .expect("wait");
        let pid = rustix::process::Pid::from_raw(self.pid as i32).expect("pid");
        let gone = matches!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        assert!(gone, "pid {} still answers after wait", self.pid);
        format!("kill={kill:?} wait={status:?} pid={} gone=true", self.pid)
    }
}

fn first_cell(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let cell = value
        .pointer("/results/0/response/result/rows/0/0")?
        .get("value")?;
    Some(match cell {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

// ═════════════════════ real Deno + SQLD ═════════════════════

const DENO_WORKER: &str = r#"
const db = Deno.env.get("SQLD_URL") ?? "";
const token = Deno.env.get("SQLD_TOKEN");
async function failure(f) {
  try { await f(); return "no-error"; } catch (e) { return e.name; }
}
async function pipeline(sql, headers) {
  const r = await fetch(new URL("/v2/pipeline", db), {
    method: "POST",
    headers: { "content-type": "application/json", ...headers },
    body: JSON.stringify({ requests: [{ type: "execute", stmt: { sql } }, { type: "close" }] }),
  });
  return { status: r.status, err: r.headers.get("x-edger-egress-error"), text: await r.text() };
}
Deno.serve(async (req) => {
  const u = new URL(req.url);
  const sql = u.searchParams.get("sql") ?? "SELECT 1";
  switch (u.pathname) {
    case "/state":
      return Response.json({
        pid: Deno.pid,
        url: db,
        tokenPresent: token !== undefined,
        tokenIsLegacy: token === "manual-legacy-value",
        socket: Deno.env.get("EDGER_EGRESS_SOCKET") !== undefined,
      });
    case "/q":
      if (!db) return Response.json({ status: 0, err: "no-sqld-url", text: "" });
      try { return Response.json(await pipeline(sql, {})); }
      catch (e) { return Response.json({ status: 0, err: e.name, text: "" }); }
    case "/q-guest":
      return Response.json(await pipeline(sql, {
        authorization: "Bearer " + (u.searchParams.get("tok") ?? ""),
        "x-namespace-bin": u.searchParams.get("ns") ?? "",
      }));
    case "/nondb": {
      const d = new URL(db);
      return Response.json({
        rawTcp: await failure(() => Deno.connect({ hostname: d.hostname, port: Number(d.port) })),
        external: await failure(() => fetch("https://example.com/")),
      });
    }
    case "/sse": {
      let n = 0;
      const body = new ReadableStream({
        async pull(c) {
          await new Promise((r) => setTimeout(r, 100));
          c.enqueue(new TextEncoder().encode("tick-" + (n++) + "\n"));
          if (n >= 200) c.close();
        },
      });
      return new Response(body, { headers: { "content-type": "text/plain" } });
    }
    default:
      return new Response("not found", { status: 404 });
  }
});
"#;

struct DenoDir {
    _dir: tempfile::TempDir,
    cache: tempfile::TempDir,
    config: WorkerConfig,
}

fn deno_dir(worker: &WorkerRef, secrets: &[&str]) -> DenoDir {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.ts"), DENO_WORKER).unwrap();
    let mut config = worker.config.clone();
    config.worker_dir = Some(dir.path().to_path_buf());
    config.timeout_ms = 30_000;
    config.secrets = secrets.iter().map(|s| (*s).to_owned()).collect();
    DenoDir {
        _dir: dir,
        cache,
        config,
    }
}

async fn get(isolate: &mut ManagedDatabaseIsolate, config: &WorkerConfig, uri: &str) -> Value {
    let response = isolate
        .execute_fetch(request(uri), config)
        .await
        .unwrap_or_else(|e| panic!("{uri}: {e:?}"));
    assert_eq!(response.status, 200, "{uri}");
    serde_json::from_slice(response.body.as_deref().unwrap_or_default()).unwrap()
}

fn pct(sql: &str) -> String {
    sql.replace(' ', "%20")
        .replace('\'', "%27")
        .replace('*', "%2A")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real sqld 0.24.32 (EDGER_SQLD_BIN) and real Deno 2.9.8 (EDGER_DENO_BIN)"]
async fn real_deno_gets_the_binding_only_after_an_explicit_terminate() {
    let exe = std::env::var("EDGER_DENO_BIN").unwrap_or_else(|_| "deno".into());
    let out = std::process::Command::new(&exe)
        .arg("--version")
        .output()
        .unwrap_or_else(|e| panic!("real Deno is required (EDGER_DENO_BIN): {exe}: {e}"));
    assert!(out.status.success());
    eprintln!(
        "isolate-evidence: {}",
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
    );
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    let (user_base, admin_base) = (sqld.user_base.clone(), sqld.admin_base.clone());
    let outcome =
        tokio::spawn(
            async move { within("deno scenario", deno_scenario(user_base, admin_base)).await },
        )
        .await;
    let stop = sqld.shutdown().await;
    eprintln!("isolate-evidence: deno shutdown {stop}");
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
}

async fn deno_scenario(user_base: String, admin_base: String) {
    let svc = build_svc(&admin_base, &user_base);
    let (store, guard) = store_and_guard();
    let a = deno_worker("iso-a");
    let fx = deno_dir(&a, &["SQLD_TOKEN"]);
    let original = fx.config.clone();
    let mut wrapper = ManagedDatabaseIsolate::deno(
        DenoProcessIsolate::new().with_per_worker_deno_cache_root(fx.cache.path()),
        &a,
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        &user_base,
    );
    let outcome = deno_checks(&svc, &store, &guard, &a, &fx, &mut wrapper, &user_base).await;
    // Cleanup runs whether or not the checks passed.
    let report = wrapper.terminate_with_report().await;
    eprintln!("isolate-evidence: final terminate {report:?}");
    assert_eq!(fx.config, original, "the original config never changes");
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
    report.expect("terminate");
}

type Checked = Result<(), Box<dyn std::any::Any + Send>>;

async fn deno_checks(
    svc: &Svc,
    store: &OperationalStore,
    guard: &Arc<EgressGuard>,
    a: &WorkerRef,
    fx: &DenoDir,
    wrapper: &mut ManagedDatabaseIsolate,
    user_base: &str,
) -> Checked {
    std::panic::AssertUnwindSafe(async {
        let config = fx.config.clone();

        // 1. The process is born WITHOUT a binding (no registry record).
        let s0 = get(wrapper, &config, "/state").await;
        assert_eq!(s0["url"], "", "{s0}");
        assert_eq!(s0["tokenPresent"], false, "{s0}");
        assert_eq!(s0["socket"], false, "no guard socket: {s0}");
        let pid0 = s0["pid"].as_u64().unwrap();
        assert!(svc.registry.get(&reg_owner(a)).unwrap().is_none());

        // 2. The binding is ensured through the REAL service while alive.
        let ns_a = ensure_ready(svc, a).await;
        assert!(vault_token(svc, a).is_some());

        // 3. A HOT request neither gains env/socket nor changes the PID,
        //    and the DB is not reachable (no authority was armed).
        let s1 = get(wrapper, &config, "/state").await;
        assert_eq!(s1["pid"].as_u64().unwrap(), pid0, "no restart");
        assert_eq!(s1["url"], "", "{s1}");
        assert_eq!(s1["tokenPresent"], false, "{s1}");
        assert_eq!(s1["socket"], false, "{s1}");
        let q = get(wrapper, &config, "/q?sql=SELECT%201").await;
        assert_eq!(q["err"], "no-sqld-url", "hot process has no database: {q}");

        // 4. Explicit terminate; the report is the inner one.
        let report = wrapper.terminate_with_report().await.expect("terminate");
        assert_eq!(report.outcome, TerminationOutcome::Completed, "{report:?}");
        assert!(report.process_id.is_some(), "{report:?}");

        // 5. The next spawn gets the HOST config (private, from the CURRENT
        //    config clone) and a working private socket: no grant mounted
        //    here, no allowNet TCP.
        let before = config.clone();
        let s2 = get(wrapper, &config, "/state").await;
        assert_ne!(s2["pid"].as_u64().unwrap(), pid0, "a new process: {s2}");
        assert_eq!(config, before, "the config passed in is unchanged");
        assert!(
            s2["url"].as_str().unwrap().starts_with(user_base),
            "SQLD_URL is the host origin: {s2}"
        );
        assert_eq!(s2["tokenPresent"], true, "{s2}");
        assert_eq!(
            s2["socket"], true,
            "the private guard socket is armed: {s2}"
        );
        let pid2 = s2["pid"].as_u64().unwrap();
        for (i, statement) in [
            "CREATE TABLE marker (v TEXT)",
            "INSERT INTO marker VALUES ('A-data')",
        ]
        .iter()
        .enumerate()
        {
            let q = get(wrapper, &config, &format!("/q?sql={}", pct(statement))).await;
            assert_eq!(q["status"], 200, "stmt {i}: {q}");
        }
        let read = pct("SELECT v FROM marker");
        let q = get(wrapper, &config, &format!("/q?sql={read}")).await;
        assert_eq!(q["status"], 200, "{q}");
        assert_eq!(
            first_cell(q["text"].as_str().unwrap()).as_deref(),
            Some("A-data")
        );
        let nondb = get(wrapper, &config, "/nondb").await;
        assert_eq!(nondb["rawTcp"], "NotCapable", "no allowNet TCP: {nondb}");
        assert_eq!(nondb["external"], "NotCapable", "{nondb}");

        // 6. Another app's token/namespace sent by the guest changes nothing.
        let b = deno_worker("iso-b");
        let ns_b = ensure_ready(svc, &b).await;
        let tok_b = vault_token(svc, &b).unwrap();
        assert_ne!(ns_a, ns_b);
        let q = get(
            wrapper,
            &config,
            &format!("/q-guest?sql={read}&tok={tok_b}&ns={}", base64_ns(&ns_b)),
        )
        .await;
        assert_eq!(q["status"], 200, "{q}");
        assert_eq!(
            first_cell(q["text"].as_str().unwrap()).as_deref(),
            Some("A-data")
        );

        // 7. A live process ignores later metadata changes (no lookup, no
        //    re-arm, no restart): the binding turns into Conflict yet the
        //    HTTP of the process still answers, and its PID is the same;
        //    the dynamic guard denies the database access immediately.
        svc.registry.mark_conflict(&reg_owner(a)).unwrap();
        let s3 = get(wrapper, &config, "/state").await;
        assert_eq!(s3["pid"].as_u64().unwrap(), pid2, "no restart on Conflict");
        let q = get(wrapper, &config, &format!("/q?sql={read}")).await;
        assert_eq!(
            q["err"], "EGRESS_DATABASE_DENIED",
            "guard denies per access: {q}"
        );
        // ... and a NEW spawn (after terminate) is refused outright.
        wrapper.terminate().await.expect("terminate");
        let error = wrapper
            .execute_fetch(request("/state"), &config)
            .await
            .unwrap_err();
        assert_static(&error, "SQLD_BINDING_NOT_READY");

        // 8. A NAMESPACE of another app grants nothing: the same worker name
        //    under another RBAC namespace is another owner with NO record,
        //    so its process has no database authority.
        let other_ns = worker_of("iso-a", Some("@other"), ExecutionKind::FetchHandler);
        let fx2 = deno_dir(&other_ns, &[]);
        let mut foreign = ManagedDatabaseIsolate::deno(
            DenoProcessIsolate::new().with_per_worker_deno_cache_root(fx2.cache.path()),
            &other_ns,
            Arc::clone(&svc.service),
            Arc::clone(guard),
            user_base,
        );
        let r = async {
            let s = get(&mut foreign, &fx2.config, "/state").await;
            assert_eq!(s["url"], "", "{s}");
            assert_eq!(s["tokenPresent"], false, "{s}");
            let q = get(&mut foreign, &fx2.config, "/q").await;
            assert_eq!(q["err"], "no-sqld-url", "{q}");
        }
        .catch_unwind_wrapper()
        .await;
        let _ = foreign.terminate().await;
        if let Err(p) = r {
            std::panic::resume_unwind(p);
        }
        assert!(svc.registry.get(&reg_owner(&other_ns)).unwrap().is_none());

        // 9. Legacy manual SQLD_TOKEN of an owner WITHOUT a record keeps its
        //    legacy semantics (the resolver delegates to the vault).
        let legacy = deno_worker("iso-legacy");
        svc.vault
            .set(&legacy.name, SQLD_TOKEN_SECRET, "manual-legacy-value")
            .unwrap();
        let fx3 = deno_dir(&legacy, &["SQLD_TOKEN"]);
        let mut legacy_iso = ManagedDatabaseIsolate::deno(
            DenoProcessIsolate::new().with_per_worker_deno_cache_root(fx3.cache.path()),
            &legacy,
            Arc::clone(&svc.service),
            Arc::clone(guard),
            user_base,
        );
        let r = async {
            let s = get(&mut legacy_iso, &fx3.config, "/state").await;
            assert_eq!(s["tokenIsLegacy"], true, "legacy manual token: {s}");
            assert_eq!(s["url"], "", "{s}");
        }
        .catch_unwind_wrapper()
        .await;
        let _ = legacy_iso.terminate().await;
        if let Err(p) = r {
            std::panic::resume_unwind(p);
        }
        assert!(svc.registry.get(&reg_owner(&legacy)).unwrap().is_none());
        assert_eq!(
            vault_token(svc, &legacy).as_deref(),
            Some("manual-legacy-value")
        );

        // 10. Streaming stays STREAMED; abandoning the body mid-stream cleans
        //     up (the next request still works and a terminate completes).
        //     A fresh Ready owner (the first one is in Conflict by design).
        let c = deno_worker("iso-c");
        let fx4 = deno_dir(&c, &["SQLD_TOKEN"]);
        ensure_ready(svc, &c).await;
        // The inner keeps the knobs the host configured (stream-detach and
        // abandon-drain limits): the wrapper does not replace them.
        let mut streaming = ManagedDatabaseIsolate::deno(
            DenoProcessIsolate::new()
                .with_per_worker_deno_cache_root(fx4.cache.path())
                .with_stream_detach(1 << 20, Arc::new(StreamDetachBudget::new(1 << 20)))
                .with_abandon_drain_limits(1 << 20, 5_000),
            &c,
            Arc::clone(&svc.service),
            Arc::clone(guard),
            user_base,
        );
        let r = async {
            let response = streaming
                .execute_fetch_stream(request("/sse"), &fx4.config)
                .await
                .expect("stream");
            let WorkerResponse::Streamed(mut streamed) = response else {
                panic!("the Deno stream must stay streamed through the wrapper");
            };
            let first = tokio::time::timeout(Duration::from_secs(20), streamed.body.next())
                .await
                .expect("first chunk")
                .expect("chunk")
                .expect("ok chunk");
            assert!(first.starts_with(b"tick-"), "{first:?}");
            drop(streamed); // the consumer cancels mid-stream
                            // After the cancel the next request works: either on the same
                            // process (clean cancel) or, if the inner reports the old process
                            // as poisoned, on the respawn that the wrapper arms again.
            let mut attempts = 0;
            let state = loop {
                attempts += 1;
                match streaming
                    .execute_fetch(request("/state"), &fx4.config)
                    .await
                {
                    Ok(response) => {
                        break serde_json::from_slice::<Value>(
                            response.body.as_deref().unwrap_or_default(),
                        )
                        .unwrap()
                    }
                    Err(error) if attempts == 1 && error.code == "UDS_POISONED" => {
                        eprintln!("isolate-evidence: cancel poisoned the process, respawn follows");
                    }
                    Err(error) => panic!("after the cancel: {error:?}"),
                }
            };
            eprintln!("isolate-evidence: after cancel attempts={attempts}");
            assert_eq!(state["tokenPresent"], true, "{state}");
            assert!(
                state["url"].as_str().unwrap().starts_with(user_base),
                "{state}"
            );
            assert_eq!(state["socket"], true, "{state}");
            // Failure path: a request that errors mid-way is cleaned too.
            let missing = streaming
                .execute_fetch_stream(request("/missing"), &fx4.config)
                .await
                .expect("404 is a response");
            match missing {
                WorkerResponse::Buffered(r) => assert_eq!(r.status, 404),
                WorkerResponse::Streamed(s) => assert_eq!(s.status, 404),
            }
        }
        .catch_unwind_wrapper()
        .await;
        let end = streaming.terminate_with_report().await;
        if let Err(p) = r {
            std::panic::resume_unwind(p);
        }
        assert!(
            matches!(
                end.as_ref().map(|r| r.outcome),
                Ok(TerminationOutcome::Completed)
            ),
            "terminate after a cancelled stream completes: {end:?}"
        );

        // Events carry no credentials.
        let text = serde_json::to_string(&egress_events(store)).unwrap();
        assert!(!text.contains("Bearer") && !text.contains("eyJ"));
        assert!(!text.contains(&tok_b));
    })
    .catch_unwind_wrapper()
    .await
}

fn base64_ns(ns: &str) -> String {
    use base64::engine::general_purpose::STANDARD_NO_PAD;
    use base64::Engine as _;
    STANDARD_NO_PAD.encode(ns)
}

/// A tiny catch-unwind adapter so cleanup always runs.
trait CatchUnwindWrapper: Future + Sized {
    fn catch_unwind_wrapper(self) -> Unwind<Self> {
        Unwind(Box::pin(self))
    }
}
impl<F: Future> CatchUnwindWrapper for F {}

struct Unwind<F>(std::pin::Pin<Box<F>>);

impl<F: Future> Future for Unwind<F> {
    type Output = std::thread::Result<F::Output>;
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.0.as_mut().poll(cx))) {
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
            Err(payload) => std::task::Poll::Ready(Err(payload)),
        }
    }
}

// ═════════════════════ real SQLD + the approved Go artifact ═════════════════════

fn go_fixture() -> PathBuf {
    let raw = std::env::var("EDGER_GO_WASM_FIXTURE").unwrap_or_else(|_| {
        panic!("EDGER_GO_WASM_FIXTURE must name the real Go build artifact; this test never skips")
    });
    let path = PathBuf::from(&raw);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {raw}: {e}"));
    assert_eq!(&bytes[..4], b"\0asm", "not a wasm binary");
    let sha: String = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    assert_eq!(sha, GO_FIXTURE_SHA, "the Go artifact identity is immutable");
    path
}

/// The Go example manifest as shipped (`SQLD_URL: ""`, `SQLD_TOKEN` secret):
/// the host origin is supplied by the managed config, not by the manifest.
fn go_ref(name: &str, namespace: &str, wasm: &Path, dir: &Path) -> WorkerRef {
    std::fs::copy(wasm, dir.join("index.wasm")).expect("copy the Go artifact");
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../workers/examples/go-wasm-http/manifest.yaml"),
    )
    .unwrap();
    let text = text
        .replace("name: go-wasm-http\n", &format!("name: \"{name}\"\n"))
        .replace("timeout: 30s\n", "timeout: 60s\n");
    let options = serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins,
        with_snippet: false,
    };
    let manifest: WorkerManifest = serde_saphyr::from_str_with_options(&text, options).unwrap();
    let mut config = parse_worker_config(&manifest);
    config.worker_dir = Some(dir.to_path_buf());
    assert!(config.egress.is_none(), "no general egress in the manifest");
    WorkerRef {
        id: uuid::Uuid::new_v4(),
        name: name.into(),
        version: manifest.version.clone().expect("version"),
        dir: dir.to_path_buf(),
        namespace: Some(namespace.into()),
        kind: config.kind.clone().unwrap_or(ExecutionKind::FetchHandler),
        config,
    }
}

fn db_request(id: &str) -> SerializedRequest {
    SerializedRequest {
        method: "GET".into(),
        uri: "/db".into(),
        headers: vec![],
        body: None,
        request_id: id.into(),
        base_href: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real sqld 0.24.32 (EDGER_SQLD_BIN) and the approved Go artifact (EDGER_GO_WASM_FIXTURE)"]
async fn real_go_wasm_reaches_its_database_through_the_wrapper() {
    let wasm = go_fixture();
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    let (user_base, admin_base) = (sqld.user_base.clone(), sqld.admin_base.clone());
    let outcome = tokio::spawn(async move {
        within("go scenario", go_scenario(wasm, user_base, admin_base)).await
    })
    .await;
    let stop = sqld.shutdown().await;
    eprintln!("isolate-evidence: go shutdown {stop}");
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
}

async fn go_scenario(wasm: PathBuf, user_base: String, admin_base: String) {
    const DB_OK: &str = r#"{"ok":true,"value":1}"#;
    const DB_503: &str = r#"{"error":"configuration missing"}"#;
    let svc = build_svc(&admin_base, &user_base);
    let (store, guard) = store_and_guard();
    let (dir_a, dir_c) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let go_a = go_ref("go-wasm-http", "nsGo", &wasm, dir_a.path());
    let go_c = go_ref("go-wasm-http-c", "nsGoC", &wasm, dir_c.path());

    // C has NO binding: it must stay without anything, guest 503, nothing
    // seeded by the wrapper (checked before AND after A is ensured).
    let mut wrapper_c = ManagedDatabaseIsolate::wasm(
        &go_c,
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        &user_base,
    );
    let res = wrapper_c
        .execute_wasm(db_request("go-c-1"), &go_c.config)
        .await
        .expect("C /db without a binding");
    assert_eq!(res.status, 503);
    assert_eq!(res.body.as_deref(), Some(DB_503.as_bytes()));
    assert!(svc.registry.get(&reg_owner(&go_c)).unwrap().is_none());
    assert!(vault_token(&svc, &go_c).is_none(), "nothing was seeded");

    // A is ensured through the REAL service; ONE wrapper (one isolate, one
    // module cache) serves two calls.
    ensure_ready(&svc, &go_a).await;
    let mut wrapper_a = ManagedDatabaseIsolate::wasm(
        &go_a,
        Arc::clone(&svc.service),
        Arc::clone(&guard),
        &user_base,
    );
    let before = go_a.config.clone();
    let res = wrapper_a
        .execute_wasm(db_request("go-a-1"), &go_a.config)
        .await
        .expect("A /db #1");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.as_deref(), Some(DB_OK.as_bytes()));
    // The module file is gone: the second call can only succeed from the
    // bytes the SAME isolate retained (nothing is rebuilt per call).
    std::fs::remove_file(dir_a.path().join("index.wasm")).unwrap();
    let res = wrapper_a
        .execute_wasm(db_request("go-a-2"), &go_a.config)
        .await
        .expect("A /db #2 from the retained isolate");
    assert_eq!(res.status, 200);
    assert_eq!(res.body.as_deref(), Some(DB_OK.as_bytes()));
    assert_eq!(go_a.config, before, "the original config is unchanged");
    let events = egress_events(&store);
    for id in ["go-a-1", "go-a-2"] {
        let ev = events
            .iter()
            .find(|e| e.request_id.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("{id}: the guard saw the call"));
        assert_eq!(ev.outcome.as_deref(), Some("ok"));
        assert_eq!(ev.namespace.as_deref(), Some("nsGo"));
    }

    // C (still no binding) is unchanged by A's Ready state.
    let res = wrapper_c
        .execute_wasm(db_request("go-c-2"), &go_c.config)
        .await
        .expect("C /db again");
    assert_eq!(res.status, 503);
    assert!(
        events
            .iter()
            .all(|e| e.request_id.as_deref() != Some("go-c-1")),
        "no guard call for C"
    );
    assert!(svc.registry.get(&reg_owner(&go_c)).unwrap().is_none());
    // Not wired: this is the host component, not the product factory/CLI.
}

// ═════════════════════ real SQLD + WASM: no foreign token without a record ═════════════════════

/// WASI reader that NEVER returns an env value: the body is exactly two
/// bytes, `'0'`/`'1'` for "an env entry named `SQLD_TOKEN` exists" and
/// "an env entry named `OTHER` exists" (entry-start match on `NAME=`).
const TOKEN_PRESENCE_WAT: &str = r#"
    (module
      (import "wasi_snapshot_preview1" "environ_sizes_get"
        (func $sizes (param i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "environ_get"
        (func $get (param i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 512) "[[\"content-type\",\"text/plain\"]]")
      (data (i32.const 600) "SQLD_TOKEN=")
      (data (i32.const 620) "OTHER=")
      (global $heap (mut i32) (i32.const 49000))

      (func (export "edger_alloc") (param $len i32) (result i32)
        (local $ptr i32)
        global.get $heap
        local.set $ptr
        global.get $heap
        local.get $len
        i32.add
        global.set $heap
        local.get $ptr
      )

      (func $copy (param $dst i32) (param $src i32) (param $len i32)
        (local $i i32)
        loop $copy_loop
          local.get $i
          local.get $len
          i32.lt_u
          if
            local.get $dst
            local.get $i
            i32.add
            local.get $src
            local.get $i
            i32.add
            i32.load8_u
            i32.store8
            local.get $i
            i32.const 1
            i32.add
            local.set $i
            br $copy_loop
          end
        end
      )

      (func $same (param $a i32) (param $b i32) (param $len i32) (result i32)
        (local $i i32)
        block $done
          loop $l
            local.get $i
            local.get $len
            i32.ge_u
            br_if $done
            local.get $a
            local.get $i
            i32.add
            i32.load8_u
            local.get $b
            local.get $i
            i32.add
            i32.load8_u
            i32.ne
            if
              i32.const 0
              return
            end
            local.get $i
            i32.const 1
            i32.add
            local.set $i
            br $l
          end
        end
        i32.const 1
      )

      (func $has (param $pat i32) (param $plen i32) (param $size i32) (result i32)
        (local $i i32)
        block $done
          loop $l
            local.get $i
            local.get $plen
            i32.add
            local.get $size
            i32.gt_u
            br_if $done
            local.get $i
            i32.eqz
            if (result i32)
              i32.const 1
            else
              i32.const 8000
              local.get $i
              i32.add
              i32.const 1
              i32.sub
              i32.load8_u
              i32.eqz
            end
            if
              i32.const 8000
              local.get $i
              i32.add
              local.get $pat
              local.get $plen
              call $same
              if
                i32.const 1
                return
              end
            end
            local.get $i
            i32.const 1
            i32.add
            local.set $i
            br $l
          end
        end
        i32.const 0
      )

      (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
        (local $size i32)
        i32.const 1000
        i32.const 1004
        call $sizes
        i32.eqz
        if
        else
          unreachable
        end
        i32.const 1004
        i32.load
        local.set $size
        local.get $size
        i32.const 8192
        i32.gt_s
        if
          unreachable
        end
        i32.const 2000
        i32.const 8000
        call $get
        i32.eqz
        if
        else
          unreachable
        end

        i32.const 40000
        i32.const 200
        i32.store16
        i32.const 40004
        i32.const 31
        i32.store
        i32.const 40008
        i32.const 2
        i32.store
        i32.const 40012
        i32.const 512
        i32.const 31
        call $copy

        i32.const 40043
        i32.const 600
        i32.const 11
        local.get $size
        call $has
        i32.const 48
        i32.add
        i32.store8
        i32.const 40044
        i32.const 620
        i32.const 6
        local.get $size
        call $has
        i32.const 48
        i32.add
        i32.store8

        i32.const 40000
        i64.extend_i32_u
        i64.const 45
        i64.const 32
        i64.shl
        i64.or
      )
    )
"#;

struct PresenceFx {
    _dir: tempfile::TempDir,
    worker: WorkerRef,
}

fn presence_fx(name: &str, namespace: &str) -> PresenceFx {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.wat"), TOKEN_PRESENCE_WAT).unwrap();
    let mut worker = worker_of(
        name,
        Some(namespace),
        ExecutionKind::WasmModule { entry: None },
    );
    worker.config.worker_dir = Some(dir.path().to_path_buf());
    worker.config.entrypoint = Some("index.wat".into());
    worker.config.timeout_ms = 30_000;
    worker.config.secrets = vec![SQLD_TOKEN_SECRET.into(), "OTHER".into()];
    PresenceFx { _dir: dir, worker }
}

/// `(SQLD_TOKEN present, OTHER present)` as the guest saw them in WASI env.
/// The guest returns presence only, never a value.
async fn guest_presence(isolate: &mut ManagedDatabaseIsolate, worker: &WorkerRef) -> (bool, bool) {
    let response = isolate
        .execute_wasm(request("/"), &worker.config)
        .await
        .expect("the presence guest runs");
    assert_eq!(response.status, 200);
    match response.body.as_deref() {
        Some(b"00") => (false, false),
        Some(b"01") => (false, true),
        Some(b"10") => (true, false),
        Some(b"11") => (true, true),
        other => panic!(
            "unexpected presence body of {} bytes",
            other.map_or(0, <[u8]>::len)
        ),
    }
}

/// `SELECT 1` against the real SQLD namespace, authenticated by `token`.
async fn select_one(user_base: &str, ns: &str, token: &str) -> Option<String> {
    let response = direct_client()
        .post(format!("{user_base}/v2/pipeline"))
        .header("authorization", format!("Bearer {token}"))
        .header("x-namespace-bin", base64_ns(ns))
        .json(&serde_json::json!({
            "requests": [{"type": "execute", "stmt": {"sql": "SELECT 1"}}, {"type": "close"}]
        }))
        .send()
        .await
        .expect("sqld answers");
    assert_eq!(response.status().as_u16(), 200, "token A authenticates");
    first_cell(&response.text().await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires real sqld 0.24.32 (EDGER_SQLD_BIN)"]
async fn real_wasm_owner_without_a_record_never_sees_a_foreign_ready_token() {
    let dir = tempfile::tempdir().unwrap();
    let mut sqld = Sqld::start(dir.path()).await;
    let (user_base, admin_base) = (sqld.user_base.clone(), sqld.admin_base.clone());
    let outcome = tokio::spawn(async move {
        within(
            "foreign token scenario",
            foreign_token_scenario(user_base, admin_base),
        )
        .await
    })
    .await;
    let stop = sqld.shutdown().await;
    eprintln!("isolate-evidence: foreign-token shutdown {stop}");
    outcome.unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
}

async fn foreign_token_scenario(user_base: String, admin_base: String) {
    let svc = build_svc(&admin_base, &user_base);
    let (_, guard) = store_and_guard();
    // A/@t is provisioned by the REAL service; B shares the worker NAME in
    // another RBAC namespace and has NO record; M is a manual-token worker
    // with no record anywhere.
    let a = presence_fx("wm-leak", RBAC);
    let b = presence_fx("wm-leak", "@other");
    let manual = presence_fx("wm-manual", RBAC);
    let ns_a = ensure_ready(&svc, &a.worker).await;
    let token_a = vault_token(&svc, &a.worker).expect("A holds its minted token");
    assert_eq!(
        select_one(&user_base, &ns_a, &token_a).await.as_deref(),
        Some("1")
    );
    svc.vault.set("wm-leak", "OTHER", "other-fixture").unwrap();
    svc.vault
        .set("wm-manual", SQLD_TOKEN_SECRET, "manual-fixture-value")
        .unwrap();
    assert!(svc.registry.get(&reg_owner(&b.worker)).unwrap().is_none());
    assert!(svc
        .registry
        .get(&reg_owner(&manual.worker))
        .unwrap()
        .is_none());

    let wrap = |worker: &WorkerRef| {
        ManagedDatabaseIsolate::wasm(
            worker,
            Arc::clone(&svc.service),
            Arc::clone(&guard),
            &user_base,
        )
    };
    let (mut wrapper_a, mut wrapper_b, mut wrapper_m) =
        (wrap(&a.worker), wrap(&b.worker), wrap(&manual.worker));

    // Control: the Ready owner still receives its own token via WASI.
    assert_eq!(
        guest_presence(&mut wrapper_a, &a.worker).await,
        (true, true)
    );
    // The class under test: no record for B, A holds the same-name token.
    // Two executions of ONE isolate (module cache): never delivered. Other
    // secret names keep delegating to the vault.
    let first = guest_presence(&mut wrapper_b, &b.worker).await;
    let second = guest_presence(&mut wrapper_b, &b.worker).await;
    assert_eq!(first, (false, true), "SQLD_TOKEN reached B (first call)");
    assert_eq!(second, (false, true), "SQLD_TOKEN reached B (second call)");
    // Manual token without any global record is preserved.
    assert_eq!(
        guest_presence(&mut wrapper_m, &manual.worker).await,
        (true, false)
    );
    // Nothing was created for B; A's control is unchanged afterwards.
    assert!(svc.registry.get(&reg_owner(&b.worker)).unwrap().is_none());
    assert_eq!(
        guest_presence(&mut wrapper_a, &a.worker).await,
        (true, true)
    );
    assert_eq!(
        vault_token(&svc, &a.worker).as_deref(),
        Some(token_a.as_str())
    );
}
