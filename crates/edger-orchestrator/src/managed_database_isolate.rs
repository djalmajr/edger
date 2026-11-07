//! EDG-54 (15d): the asynchronous isolate wrapper of the managed SQLD
//! binding.
//!
//! [`ManagedDatabaseIsolate`] implements EVERY method of the core
//! [`Isolate`] trait over ONE retained concrete backend (the same
//! `DenoProcessIsolate` / `WasmIsolate` / bridge object, its module cache and
//! its process). The constructors are synchronous and purely assemble:
//! no metadata read, no SQLite, no crypto, no environment, no HTTP and no
//! process. All lookups (binding metadata, config application, arming of the
//! Deno private socket) happen in the async operations.
//!
//! # Identity and config
//!
//! The identity is the exact host [`WorkerRef`] given to the constructor
//! (full name, version, RBAC namespace). The constructor does NOT keep its
//! `worker.config`: every operation receives the CURRENT [`WorkerConfig`]
//! from the pool and the pure helper
//! ([`apply_managed_database_config`]) is applied to a CLONE of it. The
//! original config, the manifest, the index and the `WorkerRef` never change.
//! The WASM isolate is assembled from the original `WorkerRef` (no helper at
//! construction); each operation sets the guest env on that SAME isolate from
//! the current config (`WasmIsolate::set_worker_env_from_config`).
//!
//! # Per backend
//!
//! * Deno WITHOUT a live process: `service.metadata(owner)`. Absent keeps the
//!   original config and disarms the private socket; `Ready` applies the
//!   helper to the current config and arms
//!   `set_managed_database_egress` (listener sink, identity and origin of the
//!   host); `Intent`/`Proven`/`Conflict`, a lookup error or a config conflict
//!   fail closed with a static error BEFORE any process exists.
//! * Deno WITH a live process: no lookup, no re-arm, no restart; the call is
//!   delegated with the current config. A new binding applies only after the
//!   process is terminated or recycled; the dynamic guard keeps checking
//!   `Ready`/token on every database access.
//! * WASM: ONE isolate is assembled once
//!   (`assemble_wasm_isolate_with_managed_database`); each backend operation
//!   looks the binding up (absent: original config; `Ready`: helper clone;
//!   anything else denies).
//! * Bridge: absent delegates the legacy behavior; ANY registry record
//!   refuses backend execution (no injection, env, socket or direct network
//!   fallback).
//! * `serve_static_spa`, `notify_idle`, `terminate` and
//!   `terminate_with_report` delegate exactly (original config, same report).
//!
//! # Errors
//!
//! Only static [`IsolationError`]s: `SQLD_BINDING_UNAVAILABLE`,
//! `SQLD_BINDING_NOT_READY`, `SQLD_CONFIG_CONFLICT` and
//! `SQLD_MANAGED_RUNTIME_UNSUPPORTED`. No URL, owner, token or worker name is
//! ever in a message, in `Debug` (the type has none) or in a log.
//!
//! # Limits
//!
//! The decision is made when the operation starts: between the lookup and the
//! spawn a host fence rechecks the binding after secret resolution. A hot
//! process retains its original admission. Nothing here maps errors to HTTP
//! or wires the factory/CLI.

use std::sync::Arc;

use async_trait::async_trait;
use edger_core::{
    parse_worker_config, AppSecretResolver, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, TerminationReport, WorkerConfig, WorkerManifest, WorkerRef, WorkerResponse,
};
use edger_isolation::{DenoProcessIsolate, DenoSpawnValidator, WasmIsolate};

use crate::egress_guard::EgressGuard;
use crate::egress_wasm_bridge::assemble_wasm_isolate_with_managed_database;
use crate::managed_database_config::{apply_managed_database_config, ManagedDatabaseConfigError};
use crate::managed_database_egress::ManagedDatabaseEgress;
use crate::sqld_provision::{SqldOwner, SqldProvisionService};
use crate::sqld_registry::SqldRegistryState;

const UNAVAILABLE: &str = "SQLD_BINDING_UNAVAILABLE";
const NOT_READY: &str = "SQLD_BINDING_NOT_READY";
const CONFLICT: &str = "SQLD_CONFIG_CONFLICT";
const UNSUPPORTED: &str = "SQLD_MANAGED_RUNTIME_UNSUPPORTED";
const BINDING_CHANGED: &str = "SQLD_BINDING_CHANGED";

fn unavailable() -> IsolationError {
    IsolationError::new(UNAVAILABLE, "managed database binding is unavailable")
}

fn not_ready() -> IsolationError {
    IsolationError::new(NOT_READY, "managed database binding is not ready")
}

fn conflict() -> IsolationError {
    IsolationError::new(
        CONFLICT,
        "managed database configuration conflicts with the worker configuration",
    )
}

fn unsupported() -> IsolationError {
    IsolationError::new(
        UNSUPPORTED,
        "managed database is not supported by this runtime",
    )
}

fn binding_changed() -> IsolationError {
    IsolationError::new(
        BINDING_CHANGED,
        "managed database binding changed during spawn admission",
    )
}

#[derive(Clone, Copy)]
enum SpawnMode {
    Absent,
    Ready,
}

struct ManagedSpawnValidator {
    service: Arc<SqldProvisionService>,
    owner: SqldOwner,
    identity: WorkerRef,
    origin: String,
    mode: SpawnMode,
}

#[async_trait]
impl DenoSpawnValidator for ManagedSpawnValidator {
    async fn validate(&self, config: &WorkerConfig) -> Result<(), IsolationError> {
        let metadata = self
            .service
            .metadata(&self.owner)
            .await
            .map_err(|_| unavailable())?;
        match (self.mode, metadata) {
            (SpawnMode::Absent, None) => Ok(()),
            (SpawnMode::Absent, Some(_)) | (SpawnMode::Ready, None) => Err(binding_changed()),
            (SpawnMode::Ready, Some(metadata)) if metadata.state != SqldRegistryState::Ready => {
                Err(not_ready())
            }
            (SpawnMode::Ready, Some(_)) => {
                let applied = apply_current(&self.identity, config, &self.origin)?;
                if &applied != config {
                    return Err(conflict());
                }
                Ok(())
            }
        }
    }
}

/// The retained concrete backend. `Wasm(None)` is the static-error shape: no
/// isolate is built when the identity/origin failed the pure assembly.
enum Backend {
    Deno {
        inner: Box<DenoProcessIsolate>,
        egress: Option<ManagedDatabaseEgress>,
    },
    Wasm(Option<Box<WasmIsolate>>),
    Bridge(Box<dyn Isolate>),
}

/// See the module docs. Deliberately without `Debug`: it holds the service,
/// the guard (through the egress object) and the backend.
pub struct ManagedDatabaseIsolate {
    backend: Backend,
    /// Host identity; its `config` is a PLACEHOLDER (never read): the
    /// operations always use the config the pool passes.
    identity: WorkerRef,
    owner: SqldOwner,
    service: Arc<SqldProvisionService>,
    origin: String,
    /// Retained static error of the pure assembly (identity/origin).
    assembly_failed: bool,
}

/// The identity of `worker` with a placeholder config (the constructor never
/// keeps the original `worker.config`).
fn identity_of(worker: &WorkerRef) -> WorkerRef {
    WorkerRef {
        id: worker.id,
        name: worker.name.clone(),
        version: worker.version.clone(),
        dir: worker.dir.clone(),
        namespace: worker.namespace.clone(),
        kind: worker.kind.clone(),
        config: parse_worker_config(&WorkerManifest::default()),
    }
}

impl ManagedDatabaseIsolate {
    fn assemble(
        backend: Backend,
        worker: &WorkerRef,
        service: Arc<SqldProvisionService>,
        origin: &str,
        assembly_failed: bool,
    ) -> Self {
        Self {
            backend,
            identity: identity_of(worker),
            owner: SqldOwner::new(worker.name.clone(), worker.namespace.clone()),
            service,
            origin: origin.to_owned(),
            assembly_failed,
        }
    }

    /// Wrap a Deno process isolate without I/O. Cold admission selects its
    /// secret resolver, validation fence and socket; other settings are kept.
    /// An invalid identity/origin fails every async backend operation.
    pub fn deno(
        inner: DenoProcessIsolate,
        worker: &WorkerRef,
        service: Arc<SqldProvisionService>,
        guard: Arc<EgressGuard>,
        origin: &str,
    ) -> Self {
        match ManagedDatabaseEgress::new(Arc::clone(&service), guard, worker, origin) {
            Ok(egress) => Self::assemble(
                Backend::Deno {
                    inner: Box::new(inner),
                    egress: Some(egress),
                },
                worker,
                service,
                origin,
                false,
            ),
            Err(_) => Self::assemble(
                Backend::Deno {
                    inner: Box::new(inner),
                    egress: None,
                },
                worker,
                service,
                origin,
                true,
            ),
        }
    }

    /// Build the WASM isolate ONCE (never per call) with the service's
    /// resolver and the SAME guard / `ManagedDatabaseEgress`. Synchronous and
    /// free of I/O.
    ///
    /// The isolate is assembled from the ORIGINAL `worker`: no binding helper
    /// runs here, so nothing is anticipated into its env. Each async backend
    /// operation, after the lookup and a valid config, sets the guest env of
    /// that execution on the same isolate from the CURRENT config (unchanged
    /// when there is no record, the helper clone when `Ready`).
    pub fn wasm(
        worker: &WorkerRef,
        service: Arc<SqldProvisionService>,
        guard: Arc<EgressGuard>,
        origin: &str,
    ) -> Self {
        let owner = SqldOwner::new(worker.name.clone(), worker.namespace.clone());
        let Ok(egress) =
            ManagedDatabaseEgress::new(Arc::clone(&service), Arc::clone(&guard), worker, origin)
        else {
            return Self::assemble(Backend::Wasm(None), worker, service, origin, true);
        };
        let resolver: Arc<dyn AppSecretResolver> = service.resolver_for(owner);
        let isolate = assemble_wasm_isolate_with_managed_database(
            worker,
            Some(resolver),
            Some(guard),
            Some(&egress),
        );
        Self::assemble(
            Backend::Wasm(Some(Box::new(isolate))),
            worker,
            service,
            origin,
            false,
        )
    }

    /// Wrap a legacy bridge isolate: it only runs for an owner WITHOUT any
    /// registry record; any record refuses backend execution.
    pub fn bridge(
        inner: Box<dyn Isolate>,
        worker: &WorkerRef,
        service: Arc<SqldProvisionService>,
    ) -> Self {
        Self::assemble(Backend::Bridge(inner), worker, service, "", false)
    }

    /// Decides one backend operation: `Ok(None)` runs with the ORIGINAL
    /// config, `Ok(Some(config))` with the helper clone. Never executes.
    async fn gate(
        &mut self,
        config: &WorkerConfig,
    ) -> Result<Option<WorkerConfig>, IsolationError> {
        if self.assembly_failed {
            return Err(unavailable());
        }
        match &mut self.backend {
            Backend::Bridge(_) => match self.service.metadata(&self.owner).await {
                Err(_) => Err(unavailable()),
                Ok(None) => Ok(None),
                Ok(Some(_)) => Err(unsupported()),
            },
            Backend::Deno { inner, egress } => {
                if inner.has_live_process() {
                    return Ok(None);
                }
                let lookup = self.service.metadata(&self.owner).await;
                // No process exists here, so disarming cannot be refused.
                let disarm = |inner: &mut DenoProcessIsolate| {
                    inner
                        .clear_managed_database_egress()
                        .map_err(|_| unavailable())
                };
                let metadata = match lookup {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        disarm(inner)?;
                        return Err(unavailable());
                    }
                };
                let Some(metadata) = metadata else {
                    disarm(inner)?;
                    inner
                        .set_app_secret_resolver(
                            self.identity.name.clone(),
                            self.service.resolver_for_legacy_spawn(self.owner.clone()),
                        )
                        .map_err(|_| unavailable())?;
                    inner
                        .set_spawn_validator(Some(Arc::new(ManagedSpawnValidator {
                            service: Arc::clone(&self.service),
                            owner: self.owner.clone(),
                            identity: self.identity.clone(),
                            origin: self.origin.clone(),
                            mode: SpawnMode::Absent,
                        })))
                        .map_err(|_| unavailable())?;
                    return Ok(None);
                };
                if metadata.state != SqldRegistryState::Ready {
                    disarm(inner)?;
                    return Err(not_ready());
                }
                let applied = match apply_current(&self.identity, config, &self.origin) {
                    Ok(applied) => applied,
                    Err(error) => {
                        disarm(inner)?;
                        return Err(error);
                    }
                };
                let Some(egress) = egress.as_ref() else {
                    disarm(inner)?;
                    return Err(unavailable());
                };
                inner
                    .set_app_secret_resolver(
                        self.identity.name.clone(),
                        self.service.resolver_for(self.owner.clone()),
                    )
                    .map_err(|_| unavailable())?;
                inner
                    .set_spawn_validator(Some(Arc::new(ManagedSpawnValidator {
                        service: Arc::clone(&self.service),
                        owner: self.owner.clone(),
                        identity: self.identity.clone(),
                        origin: self.origin.clone(),
                        mode: SpawnMode::Ready,
                    })))
                    .map_err(|_| unavailable())?;
                inner
                    .set_managed_database_egress(
                        egress.listener_sink(),
                        self.identity.name.clone(),
                        self.identity.version.clone(),
                        self.identity.namespace.clone(),
                        self.origin.clone(),
                    )
                    .map_err(|_| unavailable())?;
                Ok(Some(applied))
            }
            Backend::Wasm(inner) => {
                let applied = match self.service.metadata(&self.owner).await {
                    Err(_) => return Err(unavailable()),
                    Ok(None) => None,
                    Ok(Some(metadata)) if metadata.state == SqldRegistryState::Ready => {
                        Some(apply_current(&self.identity, config, &self.origin)?)
                    }
                    Ok(Some(_)) => return Err(not_ready()),
                };
                // The guest env of THIS execution is the current config's
                // (the helper clone when Ready), set on the SAME isolate.
                let Some(isolate) = inner.as_mut() else {
                    return Err(unavailable());
                };
                isolate.set_worker_env_from_config(applied.as_ref().unwrap_or(config));
                Ok(applied)
            }
        }
    }

    /// The retained backend as the core trait object, or the static error.
    fn target(&mut self) -> Result<&mut dyn Isolate, IsolationError> {
        match &mut self.backend {
            Backend::Deno { inner, .. } => Ok(inner.as_mut()),
            Backend::Wasm(Some(inner)) => Ok(inner.as_mut()),
            Backend::Wasm(None) => Err(unavailable()),
            Backend::Bridge(inner) => Ok(inner.as_mut()),
        }
    }
}

/// Applies the pure helper to a CLONE of the CURRENT config.
fn apply_current(
    identity: &WorkerRef,
    current: &WorkerConfig,
    origin: &str,
) -> Result<WorkerConfig, IsolationError> {
    let worker = WorkerRef {
        config: current.clone(),
        ..identity.clone()
    };
    apply_managed_database_config(&worker, origin).map_err(|error| match error {
        ManagedDatabaseConfigError::InvalidHostOrigin => unavailable(),
        _ => conflict(),
    })
}

#[async_trait]
impl Isolate for ManagedDatabaseIsolate {
    async fn prepare(&mut self, config: &WorkerConfig) -> Result<(), IsolationError> {
        let applied = self.gate(config).await?;
        self.target()?
            .prepare(applied.as_ref().unwrap_or(config))
            .await
    }

    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let applied = self.gate(config).await?;
        self.target()?
            .execute_fetch(req, applied.as_ref().unwrap_or(config))
            .await
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let applied = self.gate(config).await?;
        self.target()?
            .execute_routes(req, applied.as_ref().unwrap_or(config))
            .await
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        base_href: Option<&str>,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.target()?
            .serve_static_spa(path, base_href, config)
            .await
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let applied = self.gate(config).await?;
        self.target()?
            .execute_wasm(req, applied.as_ref().unwrap_or(config))
            .await
    }

    async fn execute_fetch_stream(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        let applied = self.gate(config).await?;
        self.target()?
            .execute_fetch_stream(req, applied.as_ref().unwrap_or(config))
            .await
    }

    async fn execute_routes_stream(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<WorkerResponse, IsolationError> {
        let applied = self.gate(config).await?;
        self.target()?
            .execute_routes_stream(req, applied.as_ref().unwrap_or(config))
            .await
    }

    fn take_response_truncation(&mut self) -> bool {
        // The host mark lives in the retained backend; a failed `target()`
        // (unavailable backend) executed nothing, so there is nothing to take.
        self.target()
            .is_ok_and(|backend| backend.take_response_truncation())
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        self.target()?.notify_idle().await
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        self.target()?.terminate().await
    }

    async fn terminate_with_report(&mut self) -> Result<TerminationReport, IsolationError> {
        self.target()?.terminate_with_report().await
    }
}

#[cfg(all(test, unix))]
mod cold_spawn_race_tests {
    use super::*;
    use crate::app_secrets::AppSecretVault;
    use crate::egress_guard::EgressGuardConfig;
    use crate::sqld_admin::SqldAdminClient;
    use crate::sqld_backup::SqldBackupStore;
    use crate::sqld_provision::{SqldSystemClock, SQLD_TOKEN_SECRET};
    use crate::sqld_registry::{SqldProvisionRegistry, SqldRegistryState, MIN_QUOTA_BYTES};
    use edger_core::ExecutionKind;
    use std::os::unix::fs::PermissionsExt;
    use std::panic::AssertUnwindSafe;
    use std::time::Duration;
    use zeroize::Zeroizing;

    struct Catch<F>(std::pin::Pin<Box<F>>);

    impl<F: std::future::Future> std::future::Future for Catch<F> {
        type Output = std::thread::Result<F::Output>;

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            match std::panic::catch_unwind(AssertUnwindSafe(|| self.0.as_mut().poll(cx))) {
                Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
                Ok(std::task::Poll::Ready(value)) => std::task::Poll::Ready(Ok(value)),
                Err(payload) => std::task::Poll::Ready(Err(payload)),
            }
        }
    }

    fn catch<F: std::future::Future>(future: F) -> Catch<F> {
        Catch(Box::pin(future))
    }

    const ADMIN_KEY: &str = "canary-admin-managed-isolate";
    const LIMIT: Duration = Duration::from_secs(30);

    async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(LIMIT, future)
            .await
            .expect("fixture operation exceeded 30 seconds")
    }

    async fn port() -> u16 {
        tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture port")
            .local_addr()
            .expect("fixture address")
            .port()
    }

    fn required_file(name: &str) -> String {
        let value = std::env::var(name).unwrap_or_else(|_| panic!("{name} is required"));
        assert!(
            std::path::Path::new(&value).is_file(),
            "{name} must name a file"
        );
        value
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires EDGER_SQLD_BIN (sqld 0.24.32) and EDGER_DENO_BIN (Deno 2.9.8)"]
    async fn cold_deno_rejects_binding_created_after_absent_gate() {
        real_cold_spawn_case(false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires EDGER_SQLD_BIN (sqld 0.24.32) and EDGER_DENO_BIN (Deno 2.9.8)"]
    async fn legacy_deno_does_not_receive_a_different_rbac_owners_managed_token() {
        real_cold_spawn_case(true).await;
    }

    async fn real_cold_spawn_case(cross_namespace: bool) {
        let sqld_bin = required_file("EDGER_SQLD_BIN");
        let _deno_bin = required_file("EDGER_DENO_BIN");
        let root = tempfile::tempdir().expect("fixture root");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .expect("private fixture root");
        for name in ["ledger", "backups", "worker", "cache"] {
            let path = root.path().join(name);
            std::fs::create_dir(&path).expect("fixture directory");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .expect("private fixture directory");
        }
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).expect("fixture key");
        let public_key = root.path().join("global.pub");
        std::fs::write(&public_key, key.public_key_pem()).expect("fixture public key");
        let user_port = port().await;
        let admin_port = port().await;
        let user_base = format!("http://127.0.0.1:{user_port}");
        let admin_base = format!("http://127.0.0.1:{admin_port}");
        let mut child = tokio::process::Command::new(sqld_bin)
            .arg("--db-path")
            .arg(root.path().join("data.sqld"))
            .arg("--http-listen-addr")
            .arg(format!("127.0.0.1:{user_port}"))
            .arg("--admin-listen-addr")
            .arg(format!("127.0.0.1:{admin_port}"))
            .arg("--admin-auth-key")
            .arg(ADMIN_KEY)
            .arg("--enable-namespaces")
            .arg("--disable-default-namespace")
            .arg("--auth-jwt-key-file")
            .arg(&public_key)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("RUST_LOG", "error")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("start fixture sqld");
        let pid = child.id().expect("fixture sqld pid");

        let outcome = catch(async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("fixture client");
            bounded(async {
                loop {
                    assert!(
                        child.try_wait().expect("sqld status").is_none(),
                        "sqld exited"
                    );
                    if let Ok(response) = client
                        .get(format!("{admin_base}/v1/namespaces/edger_probe/config"))
                        .header("authorization", format!("Basic {ADMIN_KEY}"))
                        .send()
                        .await
                    {
                        assert_eq!(response.status().as_u16(), 404, "sqld admin gate");
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await;

            let registry = Arc::new(
                SqldProvisionRegistry::open(
                    &root.path().join("ledger/ledger.db"),
                    Zeroizing::new([0x42; 32]),
                    16 * MIN_QUOTA_BYTES,
                )
                .expect("fixture registry"),
            );
            let vault = Arc::new(
                AppSecretVault::open(root.path().join("vault.db"), Zeroizing::new([0x24; 32]))
                    .expect("fixture vault"),
            );
            let admin = SqldAdminClient::new(&admin_base, Zeroizing::new(ADMIN_KEY.to_owned()))
                .expect("fixture admin");
            let backup = SqldBackupStore::new(&user_base, root.path().join("backups"))
                .expect("fixture backup");
            let service = Arc::new(
                SqldProvisionService::new(
                    registry,
                    vault,
                    admin,
                    backup,
                    &user_base,
                    Arc::new(SqldSystemClock),
                )
                .expect("fixture service"),
            );
            let marker = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("import marker");
            let marker_addr = marker.local_addr().expect("marker address");
            let manifest = WorkerManifest {
                name: "cold-race".into(),
                entrypoint: Some("index.ts".into()),
                allow_net: Some(vec![
                    format!("127.0.0.1:{user_port}"),
                    marker_addr.to_string(),
                ]),
                secrets: Some(vec![SQLD_TOKEN_SECRET.into()]),
                ..WorkerManifest::default()
            };
            let mut config = parse_worker_config(&manifest);
            config.worker_dir = Some(root.path().join("worker"));
            config.timeout_ms = 30_000;
            let worker = WorkerRef {
                id: uuid::Uuid::new_v4(),
                name: manifest.name.clone(),
                version: "1.0.0".into(),
                dir: root.path().join("worker"),
                namespace: Some((if cross_namespace { "@other" } else { "@t" }).into()),
                kind: ExecutionKind::FetchHandler,
                config: config.clone(),
            };
            let owner = SqldOwner::new(worker.name.clone(), worker.namespace.clone());
            let owner_a = SqldOwner::new(worker.name.clone(), Some("@t".into()));
            let guard = Arc::new(EgressGuard::new(EgressGuardConfig::default()));
            let mut wrapper = ManagedDatabaseIsolate::deno(
                DenoProcessIsolate::new()
                    .with_per_worker_deno_cache_root(root.path().join("cache")),
                &worker,
                Arc::clone(&service),
                guard,
                &user_base,
            );

            let mut deno_pid: Option<i32> = None;
            let result = catch(async {
                let already_ensured = if cross_namespace {
                    Some(
                        bounded(service.ensure(&owner_a, MIN_QUOTA_BYTES))
                            .await
                            .unwrap_or_else(|_| panic!("fixture ensure owner A failed")),
                    )
                } else {
                    None
                };
                assert!(bounded(service.metadata(&owner))
                    .await
                    .expect("metadata")
                    .is_none());
                assert!(bounded(wrapper.gate(&config))
                    .await
                    .expect("absent gate")
                    .is_none());

                let ensured = if let Some(ensured) = already_ensured {
                    ensured
                } else {
                    bounded(service.ensure(&owner, MIN_QUOTA_BYTES))
                        .await
                        .unwrap_or_else(|_| panic!("fixture ensure failed"))
                };
                assert_eq!(ensured.metadata.state, SqldRegistryState::Ready);
                if cross_namespace {
                    use base64::Engine as _;
                    let resolver = service.resolver_for(owner_a.clone());
                    let token = bounded(resolver.resolve_secret(&worker.name, SQLD_TOKEN_SECRET))
                        .await
                        .expect("fixture owner A token");
                    let namespace = base64::engine::general_purpose::STANDARD_NO_PAD
                        .encode(&ensured.metadata.sql_namespace);
                    let response = bounded(
                        client
                            .post(format!("{user_base}/v2/pipeline"))
                            .bearer_auth(token)
                            .header("x-namespace-bin", namespace)
                            .json(&serde_json::json!({
                                "requests": [
                                    {"type":"execute","stmt":{"sql":"SELECT 1"}},
                                    {"type":"close"}
                                ]
                            }))
                            .send(),
                    )
                    .await
                    .unwrap_or_else(|_| panic!("owner A control request failed"));
                    assert_eq!(response.status().as_u16(), 200, "owner A control status");
                    let body: serde_json::Value = bounded(response.json())
                        .await
                        .unwrap_or_else(|_| panic!("owner A control response invalid"));
                    assert_eq!(
                        body.pointer("/results/0/response/result/rows/0/0/value")
                            .and_then(serde_json::Value::as_str),
                        Some("1"),
                        "owner A managed token control"
                    );
                }
                let db = serde_json::to_string(&user_base).expect("origin json");
                let namespace =
                    serde_json::to_string(&ensured.metadata.sql_namespace).expect("namespace json");
                let source = format!(
                    r#"const db = {db};
const ns = {namespace};
fetch("http://{marker_addr}/import").catch(() => {{}});
Deno.serve(async () => {{
  const token = Deno.env.get("SQLD_TOKEN");
  let valid = false;
  let sqlStatus = 0;
  try {{
    const response = await fetch(db + "/v2/pipeline", {{
      method: "POST",
      headers: {{ "content-type": "application/json", "authorization": "Bearer " + token,
                  "x-namespace-bin": btoa(ns).replace(/=+$/, "") }},
      body: JSON.stringify({{ requests: [{{ type: "execute", stmt: {{ sql: "SELECT 1" }} }},
                                         {{ type: "close" }}] }})
    }});
    sqlStatus = response.status;
    const body = await response.json();
    valid = response.ok && body?.results?.[0]?.response?.result?.rows?.[0]?.[0]?.value === "1";
  }} catch (_) {{}}
  return Response.json({{ tokenPresent: token !== undefined, tokenValid: valid,
    socketPresent: Deno.env.get("EDGER_EGRESS_SOCKET") !== undefined, pid: Deno.pid,
    sqlStatus }});
}});"#
                );
                std::fs::write(root.path().join("worker/index.ts"), source).expect("guest fixture");

                let Backend::Deno { inner, .. } = &mut wrapper.backend else {
                    panic!("expected Deno backend");
                };
                let prepared = bounded(inner.prepare(&config)).await;
                let code = prepared.as_ref().err().map(|error| error.code.clone());
                let live = inner.has_live_process();
                let (observation, sql_status) = if prepared.is_ok() {
                    let request = SerializedRequest {
                        method: "GET".into(),
                        uri: "/".into(),
                        headers: vec![],
                        body: None,
                        request_id: "cold-spawn-race".into(),
                        base_href: None,
                    };
                    let response = bounded(inner.execute_fetch(request, &config))
                        .await
                        .unwrap_or_else(|error| panic!("guest request failed: {}", error.code));
                    assert_eq!(response.status, 200);
                    let value: serde_json::Value =
                        serde_json::from_slice(response.body.as_deref().expect("guest response"))
                            .expect("boolean guest response");
                    deno_pid = Some(
                        i32::try_from(value["pid"].as_u64().expect("fixture pid"))
                            .expect("pid range"),
                    );
                    (
                        Some((
                            value["tokenPresent"].as_bool().expect("tokenPresent"),
                            value["tokenValid"].as_bool().expect("tokenValid"),
                            value["socketPresent"].as_bool().expect("socketPresent"),
                        )),
                        Some(value["sqlStatus"].as_u64().expect("sqlStatus")),
                    )
                } else {
                    (None, None)
                };
                let imported = tokio::time::timeout(Duration::from_secs(1), marker.accept())
                    .await
                    .is_ok_and(|result| result.is_ok());
                (code, live, observation, imported, sql_status)
            })
            .await;

            let terminated = bounded(wrapper.terminate()).await;
            if let Some(raw_pid) = deno_pid {
                let pid = rustix::process::Pid::from_raw(raw_pid).expect("fixture Deno pid");
                bounded(async {
                    loop {
                        if matches!(
                            rustix::process::test_kill_process(pid),
                            Err(rustix::io::Errno::SRCH)
                        ) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await;
            }
            let result = match result {
                Ok(value) => value,
                Err(payload) => std::panic::resume_unwind(payload),
            };
            terminated.expect("terminate fixture Deno");
            result
        })
        .await;

        let kill = child.start_kill();
        let reaped = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        assert!(kill.is_ok(), "stop fixture sqld");
        assert!(matches!(reaped, Ok(Ok(_))), "reap fixture sqld");
        let pid = rustix::process::Pid::from_raw(pid as i32).expect("fixture pid");
        assert!(
            matches!(
                rustix::process::test_kill_process(pid),
                Err(rustix::io::Errno::SRCH)
            ),
            "fixture sqld still exists after wait"
        );
        let (code, live, observation, imported, sql_status) = match outcome {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        };
        if cross_namespace {
            assert!(
                code.is_none(),
                "legacy spawn must remain available: {code:?}"
            );
            assert!(live && imported, "legacy guest must execute");
            assert_eq!(
                (observation, sql_status),
                (Some((false, false, false)), Some(401)),
                "legacy guest must not receive another namespace's managed token"
            );
            return;
        }
        if code.is_none() {
            assert!(imported, "vulnerable spawn must import the guest");
            assert_eq!(
                observation,
                Some((true, true, false)),
                "vulnerable path must show valid managed token and no managed socket"
            );
        }
        assert_eq!(
            code.as_deref(),
            Some("SQLD_BINDING_CHANGED"),
            "cold spawn accepted changed binding: live={live}, guest={observation:?}"
        );
        assert!(!live, "refused spawn must not leave a live process");
        assert!(!imported, "refused spawn must not import the guest");
        assert!(
            observation.is_none(),
            "refused spawn must not import or call the guest"
        );
    }

    struct LocalFixture {
        _root: tempfile::TempDir,
        registry: Arc<SqldProvisionRegistry>,
        vault: Arc<AppSecretVault>,
        service: Arc<SqldProvisionService>,
        worker: WorkerRef,
    }

    fn local_fixture() -> LocalFixture {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        for dir in ["ledger", "backups", "worker"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
            std::fs::set_permissions(
                root.path().join(dir),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let registry = Arc::new(
            SqldProvisionRegistry::open(
                &root.path().join("ledger/ledger.db"),
                Zeroizing::new([0x42; 32]),
                16 * MIN_QUOTA_BYTES,
            )
            .unwrap(),
        );
        let vault = Arc::new(
            AppSecretVault::open(root.path().join("vault.db"), Zeroizing::new([0x24; 32])).unwrap(),
        );
        let admin =
            SqldAdminClient::new("http://127.0.0.1:1", Zeroizing::new(ADMIN_KEY.to_owned()))
                .unwrap();
        let backup =
            SqldBackupStore::new("http://127.0.0.1:2", root.path().join("backups")).unwrap();
        let service = Arc::new(
            SqldProvisionService::new(
                Arc::clone(&registry),
                Arc::clone(&vault),
                admin,
                backup,
                "http://127.0.0.1:2",
                Arc::new(SqldSystemClock),
            )
            .unwrap(),
        );
        let manifest = WorkerManifest {
            name: "race-fixture".into(),
            entrypoint: Some("index.ts".into()),
            allow_net: Some(vec!["127.0.0.1:8080".into()]),
            secrets: Some(vec!["OTHER".into(), SQLD_TOKEN_SECRET.into()]),
            ..WorkerManifest::default()
        };
        let mut config = parse_worker_config(&manifest);
        config.worker_dir = Some(root.path().join("worker"));
        let worker = WorkerRef {
            id: uuid::Uuid::new_v4(),
            name: manifest.name,
            version: "1".into(),
            dir: root.path().join("worker"),
            namespace: Some("@t".into()),
            kind: ExecutionKind::FetchHandler,
            config,
        };
        LocalFixture {
            _root: root,
            registry,
            vault,
            service,
            worker,
        }
    }

    struct IntentDuringSecret {
        registry: Arc<SqldProvisionRegistry>,
        delegate: Arc<dyn AppSecretResolver>,
        fired: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl AppSecretResolver for IntentDuringSecret {
        async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
            let value = self.delegate.resolve_secret(worker, name).await;
            if name == "OTHER" {
                let owner = crate::sqld_registry::SqldRegistryOwner {
                    worker,
                    rbac_namespace: Some("@t"),
                };
                self.registry
                    .ensure_intent(&owner, MIN_QUOTA_BYTES)
                    .unwrap();
                self.fired.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            value
        }
    }

    #[tokio::test]
    async fn binding_created_during_other_secret_is_refused_before_spawn() {
        let fixture = local_fixture();
        let owner = SqldOwner::new(
            fixture.worker.name.clone(),
            fixture.worker.namespace.clone(),
        );
        let guard = Arc::new(EgressGuard::new(EgressGuardConfig::default()));
        let mut wrapper = ManagedDatabaseIsolate::deno(
            DenoProcessIsolate::new(),
            &fixture.worker,
            Arc::clone(&fixture.service),
            guard,
            "http://127.0.0.1:2",
        );
        assert!(bounded(wrapper.gate(&fixture.worker.config))
            .await
            .unwrap()
            .is_none());
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let resolver = Arc::new(IntentDuringSecret {
            registry: Arc::clone(&fixture.registry),
            delegate: fixture.service.resolver_for_legacy_spawn(owner),
            fired: Arc::clone(&fired),
        });
        let Backend::Deno { inner, .. } = &mut wrapper.backend else {
            panic!("Deno backend")
        };
        inner
            .set_app_secret_resolver(fixture.worker.name.clone(), resolver)
            .unwrap();
        let error = bounded(inner.prepare(&fixture.worker.config))
            .await
            .unwrap_err();
        assert!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            "interleaving was reached"
        );
        assert_eq!(error.code, "SQLD_BINDING_CHANGED");
        assert!(!inner.has_live_process());
        assert_eq!(std::fs::read_dir(&fixture.worker.dir).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn legacy_resolver_preserves_manual_value_only_without_a_record() {
        let fixture = local_fixture();
        let owner = SqldOwner::new(
            fixture.worker.name.clone(),
            fixture.worker.namespace.clone(),
        );
        let resolver = fixture.service.resolver_for_legacy_spawn(owner);
        fixture
            .vault
            .set(&fixture.worker.name, SQLD_TOKEN_SECRET, "fixture-manual")
            .unwrap();
        fixture
            .vault
            .set(&fixture.worker.name, "OTHER", "fixture-other")
            .unwrap();
        assert_eq!(
            resolver
                .resolve_secret(&fixture.worker.name, SQLD_TOKEN_SECRET)
                .await
                .as_deref(),
            Some("fixture-manual")
        );
        assert_eq!(
            resolver
                .resolve_secret(&fixture.worker.name, "OTHER")
                .await
                .as_deref(),
            Some("fixture-other")
        );
        assert_eq!(
            resolver
                .resolve_secret("wrong-worker", SQLD_TOKEN_SECRET)
                .await,
            None
        );
        let invalid_owner = SqldOwner::new(fixture.worker.name.clone(), Some("bad\u{1}ns".into()));
        assert!(fixture.service.metadata(&invalid_owner).await.is_err());
        let invalid_resolver = fixture.service.resolver_for_legacy_spawn(invalid_owner);
        // The global lookup validates the worker name, not this owner's
        // namespace; an unrelated invalid namespace must not hide a manual token.
        assert_eq!(
            invalid_resolver
                .resolve_secret(&fixture.worker.name, SQLD_TOKEN_SECRET)
                .await
                .as_deref(),
            Some("fixture-manual")
        );
        let registry_owner = crate::sqld_registry::SqldRegistryOwner {
            worker: &fixture.worker.name,
            rbac_namespace: fixture.worker.namespace.as_deref(),
        };
        fixture
            .registry
            .ensure_intent(&registry_owner, MIN_QUOTA_BYTES)
            .unwrap();
        assert_eq!(
            resolver
                .resolve_secret(&fixture.worker.name, SQLD_TOKEN_SECRET)
                .await,
            None
        );
        assert_eq!(
            resolver
                .resolve_secret(&fixture.worker.name, "OTHER")
                .await
                .as_deref(),
            Some("fixture-other")
        );
        let record = fixture.registry.get(&registry_owner).unwrap().unwrap();
        fixture
            .registry
            .mark_proven(&registry_owner, record.public_key)
            .unwrap();
        fixture.registry.mark_ready(&registry_owner).unwrap();
        assert_eq!(
            resolver
                .resolve_secret(&fixture.worker.name, SQLD_TOKEN_SECRET)
                .await,
            None
        );
    }

    #[tokio::test]
    async fn legacy_resolver_refuses_foreign_records_in_each_state() {
        for (namespace, state) in [
            (Some("@other"), "intent"),
            (None, "proven"),
            (Some("@other"), "ready"),
            (None, "conflict"),
        ] {
            let fixture = local_fixture();
            let owner = SqldOwner::new(
                fixture.worker.name.clone(),
                fixture.worker.namespace.clone(),
            );
            let resolver = fixture.service.resolver_for_legacy_spawn(owner);
            fixture
                .vault
                .set(&fixture.worker.name, SQLD_TOKEN_SECRET, "fixture-manual")
                .unwrap();
            fixture
                .vault
                .set(&fixture.worker.name, "OTHER", "fixture-other")
                .unwrap();

            // A different worker with a similar name is the negative control.
            let sibling = format!("{}-other", fixture.worker.name);
            fixture
                .registry
                .ensure_intent(
                    &crate::sqld_registry::SqldRegistryOwner {
                        worker: &sibling,
                        rbac_namespace: namespace,
                    },
                    MIN_QUOTA_BYTES,
                )
                .unwrap();
            assert_eq!(
                resolver
                    .resolve_secret(&fixture.worker.name, SQLD_TOKEN_SECRET)
                    .await
                    .as_deref(),
                Some("fixture-manual")
            );

            let foreign = crate::sqld_registry::SqldRegistryOwner {
                worker: &fixture.worker.name,
                rbac_namespace: namespace,
            };
            let record = fixture
                .registry
                .ensure_intent(&foreign, MIN_QUOTA_BYTES)
                .unwrap();
            match state {
                "intent" => {}
                "proven" => {
                    fixture
                        .registry
                        .mark_proven(&foreign, record.public_key)
                        .unwrap();
                }
                "ready" => {
                    fixture
                        .registry
                        .mark_proven(&foreign, record.public_key)
                        .unwrap();
                    fixture.registry.mark_ready(&foreign).unwrap();
                }
                "conflict" => {
                    fixture.registry.mark_conflict(&foreign).unwrap();
                }
                _ => unreachable!(),
            }
            let expected = match state {
                "intent" => SqldRegistryState::Intent,
                "proven" => SqldRegistryState::Proven,
                "ready" => SqldRegistryState::Ready,
                "conflict" => SqldRegistryState::Conflict,
                _ => unreachable!(),
            };
            assert_eq!(
                fixture.registry.get(&foreign).unwrap().unwrap().state,
                expected
            );
            assert!(
                fixture
                    .service
                    .metadata(&SqldOwner::new(
                        fixture.worker.name.clone(),
                        fixture.worker.namespace.clone(),
                    ))
                    .await
                    .unwrap()
                    .is_none(),
                "current owner remains absent"
            );
            assert_eq!(
                resolver
                    .resolve_secret(&fixture.worker.name, SQLD_TOKEN_SECRET)
                    .await,
                None,
                "{state}"
            );
            assert_eq!(
                resolver
                    .resolve_secret(&fixture.worker.name, "OTHER")
                    .await
                    .as_deref(),
                Some("fixture-other")
            );
            assert_eq!(
                resolver
                    .resolve_secret("wrong-worker", SQLD_TOKEN_SECRET)
                    .await,
                None
            );
        }
    }

    #[tokio::test]
    async fn legacy_resolver_omits_token_when_global_registry_lookup_errors() {
        let fixture = local_fixture();
        let owner = SqldOwner::new(
            fixture.worker.name.clone(),
            fixture.worker.namespace.clone(),
        );
        let resolver = fixture.service.resolver_for_legacy_spawn(owner);
        fixture
            .vault
            .set(&fixture.worker.name, SQLD_TOKEN_SECRET, "fixture-manual")
            .unwrap();
        assert_eq!(
            resolver
                .resolve_secret(&fixture.worker.name, SQLD_TOKEN_SECRET)
                .await
                .as_deref(),
            Some("fixture-manual")
        );
        {
            let conn =
                rusqlite::Connection::open(fixture._root.path().join("ledger/ledger.db")).unwrap();
            conn.execute("UPDATE sqld_registry_meta SET check_nonce = x'00'", [])
                .unwrap();
        }
        assert!(fixture
            .registry
            .has_worker_name(&fixture.worker.name)
            .is_err());
        assert_eq!(
            resolver
                .resolve_secret(&fixture.worker.name, SQLD_TOKEN_SECRET)
                .await,
            None
        );
    }

    #[tokio::test]
    async fn ready_validator_checks_current_state_and_spawn_configuration() {
        let fixture = local_fixture();
        let owner = SqldOwner::new(
            fixture.worker.name.clone(),
            fixture.worker.namespace.clone(),
        );
        let validator = ManagedSpawnValidator {
            service: Arc::clone(&fixture.service),
            owner,
            identity: identity_of(&fixture.worker),
            origin: "http://127.0.0.1:2".into(),
            mode: SpawnMode::Ready,
        };
        let mut config = fixture.worker.config.clone();
        config.allow_net = Some(vec![]);
        let config = apply_current(&fixture.worker, &config, "http://127.0.0.1:2").unwrap();
        assert_eq!(
            validator.validate(&config).await.unwrap_err().code,
            "SQLD_BINDING_CHANGED"
        );
        let invalid = ManagedSpawnValidator {
            service: Arc::clone(&fixture.service),
            owner: SqldOwner::new(fixture.worker.name.clone(), Some("bad\u{1}ns".into())),
            identity: identity_of(&fixture.worker),
            origin: "http://127.0.0.1:2".into(),
            mode: SpawnMode::Ready,
        };
        assert_eq!(
            invalid.validate(&config).await.unwrap_err().code,
            "SQLD_BINDING_UNAVAILABLE"
        );
        let registry_owner = crate::sqld_registry::SqldRegistryOwner {
            worker: &fixture.worker.name,
            rbac_namespace: fixture.worker.namespace.as_deref(),
        };
        fixture
            .registry
            .ensure_intent(&registry_owner, MIN_QUOTA_BYTES)
            .unwrap();
        assert_eq!(
            validator.validate(&config).await.unwrap_err().code,
            "SQLD_BINDING_NOT_READY"
        );
        let record = fixture.registry.get(&registry_owner).unwrap().unwrap();
        fixture
            .registry
            .mark_proven(&registry_owner, record.public_key)
            .unwrap();
        fixture.registry.mark_ready(&registry_owner).unwrap();
        assert!(validator.validate(&config).await.is_ok());
        let mut hostile = config.clone();
        hostile.allow_net = Some(vec!["example.test:443".into()]);
        assert_eq!(
            validator.validate(&hostile).await.unwrap_err().code,
            "SQLD_CONFIG_CONFLICT"
        );
    }
}
