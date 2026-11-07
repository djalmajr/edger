//! Wasm/wasmtime isolate backend (`--features wasm`).
//!
//! Standalone Wasm path (not co-located in V8 isolate). Enable with:
//! `cargo check -p edger-isolation --features wasm`

mod handler;
mod http;
mod load;
mod wasi;

pub use handler::WasmHttpHandler;
pub use http::{WasmHttpError, WasmHttpRequest, WasmHttpResponse, WasmHttpTransport};
pub use load::load_wasm_from_worker_dir;
pub use wasi::WasiConfig;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use handler::{execution_deadline, WasmRuntimeLimits, WasmSecretEnv};

use async_trait::async_trait;

use edger_core::{
    is_valid_secret_name, AppSecretResolver, Isolate, IsolationError, SerializedRequest,
    SerializedResponse, WorkerConfig,
};

fn not_impl(method: &str) -> IsolationError {
    IsolationError::new(
        "NOT_IMPLEMENTED",
        format!("WasmIsolate::{method} pending wasmtime WASI runtime (see spike.md)"),
    )
}

/// (EDG-52) the shared-budget exhaustion error of the secret resolution:
/// the value and the worker identity never reach the error.
fn secret_resolution_timeout() -> IsolationError {
    IsolationError::new(
        "WASM_TIMEOUT",
        "app secret resolution timed out before the total execution budget elapsed",
    )
}

/// (EDG-52) Private app-secret binding of one isolate: the worker name
/// (from the factory's `WorkerRef`, never from headers, the URI or the
/// per-call `WorkerConfig`) plus the vault resolver. Deliberately without
/// `Debug`: the resolver may hold key material, and the values it returns
/// must not reach any formatting.
struct AppSecretsBinding {
    worker: String,
    resolver: Arc<dyn AppSecretResolver>,
}

/// Wasm isolate — `execute_wasm` uses wasmtime; other kinds remain stubbed (07.04).
pub struct WasmIsolate {
    wasi: WasiConfig,
    handler: WasmHttpHandler,
    wasm_bytes: Option<Vec<u8>>,
    /// (EDG-52) When bound, each name in the per-call `config.secrets` is
    /// resolved per execution and injected only into the WASI env of that
    /// execution's fresh Store. `None` (an embedding without a vault) never
    /// invents an app name from the request/config and reads no value.
    app_secrets: Option<AppSecretsBinding>,
    /// (EDG-52) Declared names already warned as absent for this isolate:
    /// the absence warn carries only worker/name and fires once per name.
    warned_secrets: HashSet<String>,
    /// (EDG-52 07a) The host HTTP transport for the versioned guest import
    /// `edger_http_v1.fetch`, when bound. Private and without `Debug`: the
    /// transport may hold policy material, and a grant is NEVER effective
    /// without the per-call explicit egress grant (`config.egress` is
    /// `Some`) — both are required, per execution.
    http_transport: Option<Arc<dyn http::WasmHttpTransport>>,
    /// (EDG-52 07d3a) The host's PRIVATE service transport for the versioned
    /// guest import `edger_http_v1.fetch` (a restricted-authority bridge —
    /// the 07d2b service/owner policy — never a generic internet transport).
    /// Private and without `Debug`: the transport may hold policy material or
    /// credentials, and nothing here serializes or formats it. Selected per
    /// execution ONLY when the general transport is not (see
    /// [`WasmIsolate::execute_wasm`]); it authorizes the import without a
    /// `config.egress` grant but never selects the general transport.
    service_http_transport: Option<Arc<dyn http::WasmHttpTransport>>,
}

impl WasmIsolate {
    pub fn new(wasi: WasiConfig) -> Self {
        Self {
            wasi,
            handler: WasmHttpHandler::new(),
            wasm_bytes: None,
            app_secrets: None,
            warned_secrets: HashSet::new(),
            http_transport: None,
            service_http_transport: None,
        }
    }

    /// (EDG-52) Bind the app secrets vault to this worker: each name in
    /// `config.secrets` is resolved per execution and injected ONLY into the
    /// WASI env of that execution's fresh Store — never into
    /// `WorkerConfig.env`/`public_env`, `WasiConfig` (whose derived `Debug`
    /// would leak it), this isolate's or the handler's persistent fields, the
    /// module cache, errors, logs or events.
    pub fn with_app_secrets(
        mut self,
        worker: String,
        resolver: Arc<dyn AppSecretResolver>,
    ) -> Self {
        self.app_secrets = Some(AppSecretsBinding { worker, resolver });
        self
    }

    pub fn from_worker_config(config: &WorkerConfig) -> Self {
        Self::new(WasiConfig::from_worker_config(config))
    }

    pub fn with_wasm_bytes(mut self, bytes: Vec<u8>) -> Self {
        self.wasm_bytes = Some(bytes);
        self
    }

    /// (EDG-52 07a) Bind the host HTTP transport for the versioned guest
    /// import `edger_http_v1.fetch`. The binding alone grants nothing: the
    /// import is admitted per execution ONLY with the explicit egress grant
    /// (`config.egress` is `Some` on the per-call `WorkerConfig`) AND this
    /// binding — without both, a module importing the HTTP surface fails
    /// closed with `WASM_IMPORT_DENIED` (including a module cache hit after
    /// an allowed call). The identity (worker/version/namespace) is bound by
    /// the host adapter (07b/c), never by the guest frame; the per-request
    /// `request_id` comes from the execution's `SerializedRequest`.
    pub fn with_http_transport(mut self, transport: Arc<dyn http::WasmHttpTransport>) -> Self {
        self.http_transport = Some(transport);
        self
    }

    /// (EDG-52 07d3a) Bind the host's PRIVATE service transport for the
    /// versioned guest import `edger_http_v1.fetch`: an explicit private
    /// grant to a RESTRICTED host transport (the 07d2b service/owner
    /// bridge), which is itself responsible for denying URLs outside the
    /// bound service and the wrong owner — that policy belongs to the host
    /// bridge, never to the guest configuration. This is NOT a
    /// manifest/admin API and NOT a permission granted by the guest: the
    /// host caller must bind a narrow-authority bridge, never a generic
    /// internet transport. Unlike the general binding
    /// ([`WasmIsolate::with_http_transport`]), the private binding admits
    /// the import without requiring `config.egress` to be `Some`, but it
    /// NEVER selects the general transport while
    /// `config.egress` is `None` — including a module cache hit after a
    /// call made with a general grant. The per-request `request_id` always
    /// comes from the execution's `SerializedRequest`, and there is no
    /// automatic fallback to another transport after a fetch error.
    pub fn with_service_http_transport(
        mut self,
        transport: Arc<dyn http::WasmHttpTransport>,
    ) -> Self {
        self.service_http_transport = Some(transport);
        self
    }

    /// (EDG-52) Resolves `config.secrets` for the bound worker per execution
    /// — the worker identity comes from the factory's `WorkerRef` binding,
    /// never from headers, the URI or the per-call `WorkerConfig`.
    /// Fail-closed rules:
    /// - a name failing `is_valid_secret_name` is refused before the
    ///   resolver is consulted (`WASM_SECRET_INVALID`, no value, no panic),
    ///   even for a `WorkerConfig` built directly;
    /// - a value containing a NUL is refused before any WASI construction
    ///   (`WASM_SECRET_INVALID`, the value never reaches the error);
    /// - `None` (absent or vault unavailable) leaves the variable absent,
    ///   with ONE warn per name carrying only worker/name — never a value,
    ///   size/hash or the resolver's Debug;
    /// - deadline exhaustion maps to `WASM_TIMEOUT` (the shared absolute
    ///   deadline; the resolution gets no fresh budget and the await is
    ///   cancelled, holding neither the admission permit nor a Store — the
    ///   handler has not started; a READY answer is refused both before and
    ///   after the await once the deadline elapsed, since tokio's
    ///   `timeout_at` would accept it even past the deadline).
    ///
    /// Values flow only into the per-execution `WasmSecretEnv` (no Debug):
    /// never into `WorkerConfig.env`/`public_env`, `WasiConfig`, this
    /// isolate's or the handler's persistent fields, the module cache,
    /// errors, logs or events.
    async fn resolve_app_secrets(
        &mut self,
        config: &WorkerConfig,
        deadline: Instant,
    ) -> Result<WasmSecretEnv, IsolationError> {
        let Some(binding) = &self.app_secrets else {
            // Embedding without a vault: no identity is invented from the
            // request/config and nothing is read.
            return Ok(WasmSecretEnv::empty());
        };
        if config.secrets.is_empty() {
            return Ok(WasmSecretEnv::empty());
        }
        // Validate every name before any I/O: a bad name fails closed with a
        // GENERIC error (the name itself may carry control characters or
        // sensitive data, so it is never echoed; validated names keep
        // appearing in the absent-secret warns).
        for name in &config.secrets {
            if !is_valid_secret_name(name) {
                return Err(IsolationError::new(
                    "WASM_SECRET_INVALID",
                    "a declared secret name does not match the app secret name rule",
                ));
            }
        }
        let mut secrets = WasmSecretEnv::empty();
        for name in &config.secrets {
            // (EDG-52) the shared deadline is absolute: once it elapsed the
            // resolution is refused BEFORE the resolver is consulted — no
            // fresh budget starts here.
            if Instant::now() >= deadline {
                return Err(secret_resolution_timeout());
            }
            let value = match tokio::time::timeout_at(
                deadline.into(),
                binding.resolver.resolve_secret(&binding.worker, name),
            )
            .await
            {
                Ok(value) => value,
                Err(_) => return Err(secret_resolution_timeout()),
            };
            // tokio's `timeout_at` polls the value future first and accepts a
            // READY answer even after the deadline elapsed; the explicit
            // check keeps a late answer from extending the budget.
            if Instant::now() >= deadline {
                return Err(secret_resolution_timeout());
            }
            match value {
                Some(value) if value.contains('\0') => {
                    return Err(IsolationError::new(
                        "WASM_SECRET_INVALID",
                        format!("app secret value for {name} must not contain a NUL byte"),
                    ))
                }
                Some(value) => {
                    secrets.insert(name, value);
                }
                None => {
                    if self.warned_secrets.insert(name.clone()) {
                        tracing::warn!(
                            worker = %binding.worker,
                            secret = %name,
                            "app secret not injected into the wasm worker: absent from the vault or the vault is unavailable; the worker will see the variable as absent"
                        );
                    }
                }
            }
        }
        Ok(secrets)
    }

    /// Host-only: replace the WASI env selection of this isolate with the one
    /// of `config`, using the SAME rule as [`WasiConfig::from_worker_config`]
    /// (its `env` and `allow_env`, nothing else). It does no I/O, builds
    /// nothing, reads no process env, resolves no vault value and creates no
    /// grant. `allow_fs_read`/`allow_fs_write`/`allow_net`/`allow_stdio`, the
    /// module bytes/cache, the handler, the HTTP transports and the secrets
    /// binding are untouched; secret values never enter this persistent
    /// config (they stay per-execution). Exposes no new guest capability or
    /// manifest field; the legacy runtime never calls it.
    pub fn set_worker_env_from_config(&mut self, config: &WorkerConfig) {
        let selection = WasiConfig::from_worker_config(config);
        self.wasi.env = selection.env;
        self.wasi.allow_env = selection.allow_env;
    }

    pub fn wasi_config(&self) -> &WasiConfig {
        &self.wasi
    }
}

#[async_trait]
impl Isolate for WasmIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_fetch"))
    }

    async fn execute_routes(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("execute_routes"))
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(not_impl("serve_static_spa"))
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        if self.wasm_bytes.is_none() {
            if let (Some(dir), Some(entry)) =
                (config.worker_dir.as_ref(), config.entrypoint.as_deref())
            {
                self.wasm_bytes = Some(load_wasm_from_worker_dir(dir, entry)?);
            }
        }
        if self.wasm_bytes.is_none() {
            return Err(IsolationError::new(
                "WASM_NOT_LOADED",
                "no wasm module bytes configured on WasmIsolate",
            ));
        }
        let limits = WasmRuntimeLimits::from_worker_config(config);
        // (EDG-52 07a/07d3a) the versioned HTTP import is selected per
        // execution — no privilege caching, no grant from the guest frame,
        // never memorized in the isolate/Store/cache: with the explicit
        // egress grant (the per-call config — the manifest's allowNet
        // declaration is validated by core, never read here) AND a bound
        // GENERAL host transport, that general transport is used (the
        // orchestrator may compose a general+service router there); in
        // every other case ONLY the private service transport (07d3a),
        // when bound, is used — it admits the import without the grant but
        // the general transport is NEVER selected while `config.egress` is
        // `None`, including a module cache hit after a call made with a
        // general grant. When neither applies the import fails closed with
        // WASM_IMPORT_DENIED (the handler re-validates per
        // execution/cold/cache).
        let http = match (config.egress.is_some(), self.http_transport.as_ref()) {
            (true, Some(transport)) => Some(http::WasmHttpExecution {
                transport: Arc::clone(transport),
                request_id: req.request_id.clone(),
            }),
            _ => self
                .service_http_transport
                .as_ref()
                .map(|transport| http::WasmHttpExecution {
                    transport: Arc::clone(transport),
                    request_id: req.request_id.clone(),
                }),
        };
        // (EDG-52) ONE absolute deadline for secret resolution + admission +
        // compile + guest execution: the 05b budget, never reset — a stalled
        // resolution consumes part of the same budget and maps to
        // WASM_TIMEOUT.
        let deadline = execution_deadline(&limits);
        let secrets = self.resolve_app_secrets(config, deadline).await?;
        let bytes = self.wasm_bytes.as_ref().expect("bytes checked above");
        // Async runtime egress (EDG-52 05b): guest CPU and JIT never block a
        // Tokio thread; the sync embedding API, ABI, capabilities and limits
        // are preserved (the sync handler path is untouched). The private
        // secrets live only in this execution's fresh Store WASI.
        self.handler
            .execute_module_with_config_and_limits_async(
                bytes,
                &req,
                &self.wasi,
                &limits,
                &secrets,
                deadline,
                http.as_ref(),
            )
            .await
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        Err(not_impl("notify_idle"))
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        Err(not_impl("terminate"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    use edger_core::{parse_worker_config, WorkerManifest};

    /// (test-only) the captured-warn storage type; factored into an alias
    /// (shared by `WarnCapture` and the test local) because the
    /// fully-qualified form trips clippy::type_complexity.
    type CapturedWarnEvents = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<String>)>>>;

    /// (EDG-52) Deterministic WAT fixture over the WASIp1 environ imports:
    /// `edger_handle` calls `environ_sizes_get` + `environ_get` and answers
    /// with body `[u32 count][u32 buf_size][entries]`, the entries being the
    /// NUL-terminated `NAME=VALUE` string exactly as the host wrote it into
    /// the guest-provided buffer (the host test side walks them the way a
    /// guest `getenv` would: for a duplicated name the FIRST entry wins).
    ///
    /// WASIp1 `environ_get(env_ptrs, env_buf)` ABI (wasmtime-wasi 49): the
    /// host writes ONE i32 pointer per entry into `env_ptrs` and the
    /// concatenated entries into the buffer at `env_buf` (sized by the
    /// caller from `environ_sizes_get`).
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

    /// Walks the fixture body the way a guest `getenv` would: the
    /// NUL-terminated `NAME=VALUE` entries in buffer order; for a duplicated
    /// name the FIRST entry wins.
    fn parse_env_body(body: &[u8]) -> Vec<(String, String)> {
        assert!(body.len() >= 8, "body too short: {body:?}");
        let buf_size = u32::from_le_bytes(body[4..8].try_into().expect("u32 slice")) as usize;
        assert!(
            body.len() >= 8 + buf_size,
            "body shorter than the declared env buffer: {body:?}"
        );
        let mut out = Vec::new();
        for entry in body[8..8 + buf_size].split(|&b| b == 0) {
            if entry.is_empty() {
                continue;
            }
            let entry = String::from_utf8(entry.to_vec()).expect("utf-8 entry");
            let (name, value) = entry.split_once('=').expect("NAME=VALUE entry");
            out.push((name.to_owned(), value.to_owned()));
        }
        out
    }

    fn first_value<'a>(entries: &'a [(String, String)], name: &str) -> Option<&'a str> {
        entries
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn occurrences(entries: &[(String, String)], name: &str) -> usize {
        entries.iter().filter(|(key, _)| key == name).count()
    }

    fn sample_secrets_request() -> SerializedRequest {
        SerializedRequest {
            method: "GET".into(),
            uri: "/".into(),
            headers: vec![],
            body: None,
            request_id: "wasm-secrets".into(),
            base_href: None,
        }
    }

    fn secrets_config(secrets: &[&str], env: &[(&str, &str)], timeout_ms: u64) -> WorkerConfig {
        let mut manifest = WorkerManifest {
            name: "wasm-secret-a".into(),
            kind: Some("wasm".into()),
            ..Default::default()
        };
        manifest.secrets = Some(secrets.iter().map(|name| name.to_string()).collect());
        manifest.env = Some(
            env.iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        );
        let mut config = parse_worker_config(&manifest);
        config.timeout_ms = timeout_ms;
        config
    }

    /// Deterministic in-memory resolver: exact `(worker, name)` pairs,
    /// everything else `None` (fail closed).
    struct MapResolver {
        secrets: HashMap<(String, String), String>,
    }

    impl MapResolver {
        fn new(pairs: &[(&str, &str, &str)]) -> Self {
            Self {
                secrets: pairs
                    .iter()
                    .map(|&(worker, name, value)| {
                        ((worker.to_owned(), name.to_owned()), value.to_owned())
                    })
                    .collect(),
            }
        }
    }

    #[async_trait::async_trait]
    impl AppSecretResolver for MapResolver {
        async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
            self.secrets
                .get(&(worker.to_owned(), name.to_owned()))
                .cloned()
        }
    }

    /// A resolver that never answers: `resolve_secret` awaits forever.
    /// `started` flips when consulted; `released` flips when the await
    /// future is dropped (the shared deadline cancelling the resolution).
    struct StallingResolver {
        started: Arc<AtomicBool>,
        released: Arc<AtomicBool>,
    }

    struct GateGuard {
        released: Arc<AtomicBool>,
    }

    impl Drop for GateGuard {
        fn drop(&mut self) {
            self.released.store(true, Ordering::SeqCst);
        }
    }

    impl StallingResolver {
        fn new() -> (Arc<Self>, Arc<AtomicBool>, Arc<AtomicBool>) {
            let started = Arc::new(AtomicBool::new(false));
            let released = Arc::new(AtomicBool::new(false));
            let self_arc = Arc::new(Self {
                started: Arc::clone(&started),
                released: Arc::clone(&released),
            });
            (self_arc, started, released)
        }
    }

    #[async_trait::async_trait]
    impl AppSecretResolver for StallingResolver {
        async fn resolve_secret(&self, _worker: &str, _name: &str) -> Option<String> {
            self.started.store(true, Ordering::SeqCst);
            let _guard = GateGuard {
                released: Arc::clone(&self.released),
            };
            // Never resolves: the shared deadline must cut this await.
            let _ = std::future::pending::<()>().await;
            None
        }
    }

    /// (EDG-52) Red → Green: a declared secret, resolved for the bound
    /// worker, reaches the guest WASI env of that execution's fresh Store;
    /// the public env entry is preserved and the secret is not duplicated.
    #[tokio::test]
    async fn declared_secret_appears_in_the_guest_wasi_env() {
        // ONE shared admission test lock: this execution enters the async
        // handler and takes a global permit.
        let _lock = super::handler::async_admission_test_lock().await;
        let resolver: Arc<dyn AppSecretResolver> = Arc::new(MapResolver::new(&[(
            "worker-a",
            "APP_SECRET",
            "s3cret-value",
        )]));
        let config = secrets_config(&["APP_SECRET"], &[("PUB_ONLY", "public-value")], 30_000);
        let mut isolate = WasmIsolate::new(WasiConfig::from_worker_config(&config))
            .with_app_secrets("worker-a".into(), resolver)
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());
        let res = isolate
            .execute_wasm(sample_secrets_request(), &config)
            .await
            .unwrap();
        assert_eq!(res.status, 200);
        let env = parse_env_body(res.body.unwrap().as_ref());
        assert_eq!(
            first_value(&env, "APP_SECRET"),
            Some("s3cret-value"),
            "the declared secret must reach the guest WASI env: {env:?}"
        );
        assert_eq!(first_value(&env, "PUB_ONLY"), Some("public-value"));
        assert_eq!(occurrences(&env, "APP_SECRET"), 1);
    }
    /// Inspection (getter) level: the host setter replaces ONLY the env
    /// selection (`env`, `allow_env`) with the rule of
    /// `WasiConfig::from_worker_config`; every other capability is kept.
    #[test]
    fn set_worker_env_from_config_replaces_only_the_env_selection() {
        let old = secrets_config(&[], &[("VALUE", "old")], 30_000);
        let mut wasi = WasiConfig::from_worker_config(&old);
        wasi.allow_fs_read = true;
        wasi.allow_fs_write = true;
        wasi.allow_net = true;
        wasi.allow_stdio = true;
        let mut isolate = WasmIsolate::new(wasi.clone());

        let new = secrets_config(&[], &[("VALUE", "new"), ("EXTRA", "1")], 30_000);
        isolate.set_worker_env_from_config(&new);
        let expected = WasiConfig::from_worker_config(&new);
        let got = isolate.wasi_config();
        assert_eq!(got.env, expected.env);
        assert_eq!(got.allow_env, expected.allow_env);
        assert!(got.allow_env);
        assert!(got.allow_fs_read && got.allow_fs_write && got.allow_net && got.allow_stdio);

        // An empty env follows the same rule (allow_env false, no entries).
        isolate.set_worker_env_from_config(&secrets_config(&[], &[], 30_000));
        assert!(isolate.wasi_config().env.is_empty());
        assert!(!isolate.wasi_config().allow_env);
        assert!(isolate.wasi_config().allow_fs_read && isolate.wasi_config().allow_net);
    }

    /// Executed-WASI level (guest `environ_get`): the setter changes the env
    /// the NEXT execution sees on the SAME isolate (same bytes/handler/secrets
    /// binding); a variable removed later does not return; a resolved secret
    /// is per-execution only (never in the persistent `WasiConfig`, absent
    /// once the config stops declaring it).
    #[tokio::test]
    async fn set_worker_env_from_config_is_per_execution_and_keeps_secrets_transient() {
        let _lock = super::handler::async_admission_test_lock().await;
        let resolver: Arc<dyn AppSecretResolver> = Arc::new(MapResolver::new(&[(
            "worker-a",
            "APP_SECRET",
            "s3cret-value",
        )]));
        let first = secrets_config(&["APP_SECRET"], &[("VALUE", "old")], 30_000);
        let mut isolate = WasmIsolate::new(WasiConfig::from_worker_config(&first))
            .with_app_secrets("worker-a".into(), resolver)
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());

        let guest = |res: SerializedResponse| parse_env_body(res.body.unwrap().as_ref());

        isolate.set_worker_env_from_config(&secrets_config(
            &["APP_SECRET"],
            &[("VALUE", "new")],
            30_000,
        ));
        let with_secret = secrets_config(&["APP_SECRET"], &[("VALUE", "new")], 30_000);
        let env = guest(
            isolate
                .execute_wasm(sample_secrets_request(), &with_secret)
                .await
                .unwrap(),
        );
        assert_eq!(first_value(&env, "VALUE"), Some("new"), "{env:?}");
        assert_eq!(first_value(&env, "APP_SECRET"), Some("s3cret-value"));
        let persistent = format!("{:?}", isolate.wasi_config());
        assert!(
            !persistent.contains("s3cret-value"),
            "the secret must not enter the persistent WasiConfig"
        );

        // Second execution: variable and secret declaration removed.
        let bare = secrets_config(&[], &[("OTHER", "x")], 30_000);
        isolate.set_worker_env_from_config(&bare);
        let env = guest(
            isolate
                .execute_wasm(sample_secrets_request(), &bare)
                .await
                .unwrap(),
        );
        assert_eq!(first_value(&env, "VALUE"), None, "{env:?}");
        assert_eq!(first_value(&env, "APP_SECRET"), None, "{env:?}");
        assert_eq!(first_value(&env, "OTHER"), Some("x"));
    }

    /// (EDG-52) Without a resolver binding, declared secrets resolve to
    /// nothing: no error, no identity invented from the request/config, no
    /// vault access — the execution simply sees no variable.
    #[tokio::test]
    async fn declared_secrets_without_binding_resolve_to_nothing() {
        // ONE shared admission test lock: this execution enters the async
        // handler and takes a global permit.
        let _lock = super::handler::async_admission_test_lock().await;
        let config = secrets_config(&["APP_SECRET"], &[("PUB_ONLY", "public-value")], 30_000);
        let mut isolate = WasmIsolate::new(WasiConfig::from_worker_config(&config))
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());
        let res = isolate
            .execute_wasm(sample_secrets_request(), &config)
            .await
            .expect("a missing binding must not fail the execution");
        assert_eq!(res.status, 200);
        let env = parse_env_body(res.body.unwrap().as_ref());
        assert_eq!(first_value(&env, "APP_SECRET"), None);
        assert_eq!(first_value(&env, "PUB_ONLY"), Some("public-value"));
    }

    /// (EDG-52) A stalled resolution consumes part of the SHARED absolute
    /// deadline (the 05b budget, never reset) and maps to WASM_TIMEOUT: the
    /// await is cancelled — the execution holds neither the admission permit
    /// nor a Store, and the error carries no value/worker identity.
    #[tokio::test]
    async fn stalling_secret_resolution_times_out_with_the_shared_budget() {
        let (staller, started, released) = StallingResolver::new();
        let config = secrets_config(&["APP_SECRET"], &[], 300);
        // Serialize on the admission pool and wait until it is quiescent, so
        // the post-timeout count measures THIS execution's leftovers (the
        // admission-asserting handler tests hold the same lock).
        let _pool_lock = super::handler::async_admission_test_lock().await;
        let capacity = super::handler::admission_for_test().1;
        let settle = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while super::handler::admission_for_test().0 < capacity
            && std::time::Instant::now() < settle
        {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_app_secrets("worker-slow".into(), staller)
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            isolate.execute_wasm(sample_secrets_request(), &config),
        )
        .await;
        let err = match outcome {
            Err(_) => panic!("a stalled resolution must be cut by the shared budget, not hang"),
            Ok(Err(err)) => err,
            // Static message: the Ok side (the response) is never formatted.
            Ok(_) => panic!("expected WASM_TIMEOUT, got success"),
        };
        assert_eq!(err.code, "WASM_TIMEOUT");
        assert!(err.message.is_ascii());
        assert!(
            !err.message.contains("s3cret") && !err.message.contains("worker-slow"),
            "the timeout error must not leak the worker identity or any value: {}",
            err.message
        );
        // The resolution was actually consulted and its await cancelled.
        assert!(started.load(Ordering::SeqCst));
        assert!(released.load(Ordering::SeqCst));
        // Nothing was left behind: the pool returns to full capacity — the
        // timed-out resolution holds no admission permit and the handler
        // (and thus any Store) never started.
        let settle = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut available = super::handler::admission_for_test().0;
        while available < capacity && std::time::Instant::now() < settle {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            available = super::handler::admission_for_test().0;
        }
        assert_eq!(
            available, capacity,
            "the timed-out resolution must not hold an admission permit"
        );
    }

    /// (EDG-52) A declared name failing `is_valid_secret_name` fails closed
    /// BEFORE the resolver is consulted (WASM_SECRET_INVALID, no value in
    /// the error, no panic) — even for a WorkerConfig built directly.
    #[tokio::test]
    async fn invalid_declared_secret_name_fails_closed_without_resolving() {
        let (staller, started, _released) = StallingResolver::new();
        let config = secrets_config(&["BAD NAME!!"], &[], 300);
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_app_secrets("worker-bad".into(), staller)
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            isolate.execute_wasm(sample_secrets_request(), &config),
        )
        .await;
        let err = match outcome {
            Err(_) => panic!("an invalid name must fail fast, not stall"),
            Ok(Err(err)) => err,
            // Static message: the Ok side (the response) is never formatted.
            Ok(_) => panic!("expected WASM_SECRET_INVALID"),
        };
        assert_eq!(err.code, "WASM_SECRET_INVALID");
        assert!(err.message.is_ascii());
        assert!(
            !err.message.contains("s3cret") && !err.message.contains("worker-bad"),
            "the error must not leak the worker identity or any value: {}",
            err.message
        );
        // The resolver was not consulted: no I/O on a bad name.
        assert!(!started.load(Ordering::SeqCst));
    }

    /// Captures WARN events only: the target and every recorded field. Used
    /// to prove that the absent-secret warn carries ONLY names
    /// (worker/secret) — never a value, size/hash or the resolver's Debug.
    struct WarnCapture {
        events: CapturedWarnEvents,
    }

    impl std::fmt::Debug for WarnCapture {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("WarnCapture")
        }
    }

    /// Records every field of an event as `name=value` strings (names and
    /// displayed values only — for the warn under test those are names).
    struct FieldRecorder(Vec<String>);

    impl tracing::field::Visit for FieldRecorder {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push(format!("{field}={value:?}"));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push(format!("{field}={value}"));
        }
    }

    impl tracing::Subscriber for WarnCapture {
        fn register_callsite(
            &self,
            _metadata: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::sometimes()
        }

        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() == tracing::Level::WARN
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(0xDEAD)
        }

        fn event(&self, event: &tracing::Event<'_>) {
            // `register_callsite` answers `sometimes`, but the dispatcher may
            // still skip `enabled` under optimizations: filter the level
            // here, where it cannot be bypassed — only the absent-secret
            // WARN class is of interest and the captured collection must
            // stay names-only.
            if *event.metadata().level() != tracing::Level::WARN {
                return;
            }
            let mut recorder = FieldRecorder(Vec::new());
            event.record(&mut recorder);
            self.events
                .lock()
                .unwrap()
                .push((event.metadata().target().to_owned(), recorder.0));
        }

        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    /// (EDG-52) An absent/unavailable secret fails closed: the execution
    /// continues with the variable absent, ONE warn per worker/name, and
    /// the warn carries only the worker and secret NAMES.
    #[tokio::test]
    async fn absent_secret_warns_once_with_names_only() {
        // ONE shared admission test lock: both executions enter the async
        // handler and take a global permit.
        let _lock = super::handler::async_admission_test_lock().await;
        let captured: CapturedWarnEvents = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        // The thread-local default subscriber stays installed across every
        // poll of the awaited futures (the guard lives to the end of the
        // test); a `with_default` closure dispatcher would leave scope
        // before the future is polled and the warn would never be captured.
        let _capture_guard = tracing::subscriber::set_default(WarnCapture {
            events: std::sync::Arc::clone(&captured),
        });
        let resolver: Arc<dyn AppSecretResolver> = Arc::new(MapResolver::new(&[]));
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_app_secrets("worker-absent".into(), resolver)
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());
        let config = secrets_config(&["MISSING_SECRET"], &[], 30_000);
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            isolate.execute_wasm(sample_secrets_request(), &config),
        )
        .await
        .expect("an absent secret must not hang")
        .expect("an absent secret must not fail the execution");
        assert_eq!(res.status, 200);
        let env = parse_env_body(res.body.unwrap().as_ref());
        assert_eq!(first_value(&env, "MISSING_SECRET"), None);

        // Exactly ONE warn for the absent name; every captured field is a
        // name=value pair whose value is a NAME, never a value/size/hash/
        // resolver Debug.
        let events = captured.lock().unwrap().clone();
        let warns: Vec<_> = events
            .iter()
            .filter(|(_, fields)| fields.iter().any(|field| field == "secret=MISSING_SECRET"))
            .collect();
        assert_eq!(warns.len(), 1, "one warn per worker/name");
        for (target, fields) in &warns {
            assert!(target.contains("edger_isolation"));
            assert!(fields.iter().any(|field| field == "worker=worker-absent"));
            for field in fields {
                assert!(
                    !field.contains("s3cret"),
                    "a value leaked into the warn: {field}"
                );
                assert!(!field.contains("size") && !field.contains("hash"));
            }
        }

        // A second execution of the same worker/name does not repeat the
        // warn (still under the capture guard set above).
        let before = captured.lock().unwrap().len();
        let res = isolate
            .execute_wasm(sample_secrets_request(), &config)
            .await
            .unwrap();
        assert_eq!(res.status, 200);
        // Snapshot the count into its own statement (the guard drops before
        // the assert; the message never formats the captured collection).
        let after = captured.lock().unwrap().len();
        assert_eq!(
            after, before,
            "the warn must not repeat for the same worker/name"
        );
    }

    /// A resolver whose values can be rotated or removed between executions
    /// of the SAME isolate (per-execution re-resolution; no stale reuse).
    struct MutableResolver {
        values: Arc<std::sync::Mutex<HashMap<(String, String), String>>>,
    }

    #[async_trait::async_trait]
    impl AppSecretResolver for MutableResolver {
        async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String> {
            self.values
                .lock()
                .unwrap()
                .get(&(worker.to_owned(), name.to_owned()))
                .cloned()
        }
    }

    /// (EDG-52) A secret owned by ANOTHER app never reaches this worker:
    /// the bound identity is the factory's worker name, never something
    /// invented from the request or the per-call config.
    #[tokio::test]
    async fn unrelated_app_secret_does_not_reach_the_bound_worker() {
        // ONE shared admission test lock: this execution enters the async
        // handler and takes a global permit.
        let _lock = super::handler::async_admission_test_lock().await;
        let resolver: Arc<dyn AppSecretResolver> = Arc::new(MapResolver::new(&[(
            "other-app",
            "APP_SECRET",
            "s3cret-value",
        )]));
        let config = secrets_config(&["APP_SECRET"], &[], 30_000);
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_app_secrets("worker-a".into(), resolver)
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());
        let res = isolate
            .execute_wasm(sample_secrets_request(), &config)
            .await
            .expect("an unrelated app's secret must not fail the execution");
        assert_eq!(res.status, 200);
        let env = parse_env_body(res.body.unwrap().as_ref());
        assert_eq!(
            first_value(&env, "APP_SECRET"),
            None,
            "a secret resolved for another app must not reach this worker"
        );
    }

    /// (EDG-52) A value containing a NUL fails closed with the generic
    /// `WASM_SECRET_INVALID` before any WASI construction: no panic, and the
    /// error carries neither the value nor the worker identity.
    #[tokio::test]
    async fn nul_secret_value_fails_closed_without_reaching_wasi() {
        let resolver: Arc<dyn AppSecretResolver> = Arc::new(MapResolver::new(&[(
            "worker-a",
            "NUL_SECRET",
            "nul-canary\0tail",
        )]));
        let config = secrets_config(&["NUL_SECRET"], &[], 30_000);
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_app_secrets("worker-a".into(), resolver)
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());
        let err = isolate
            .execute_wasm(sample_secrets_request(), &config)
            .await
            .expect_err("a NUL-bearing value must fail closed");
        assert_eq!(err.code, "WASM_SECRET_INVALID");
        assert!(
            !err.message.contains("nul-canary") && !err.message.contains("worker-a"),
            "the error must carry no value and no worker identity"
        );
    }

    /// (EDG-52) Rotation and removal are re-resolved PER EXECUTION on the
    /// SAME isolate: the next call sees the new value (never the old one),
    /// and a removed secret is absent — a previously injected value is never
    /// reused. The fixture also proves the secret reaches the guest with
    /// `allow_env = false` (deny-all caps: no fs/net/stdio granted).
    #[tokio::test]
    async fn rotation_and_removal_are_re_resolved_per_execution_on_the_same_isolate() {
        // ONE shared admission test lock: every execution enters the async
        // handler and takes a global permit.
        let _lock = super::handler::async_admission_test_lock().await;
        let values: Arc<std::sync::Mutex<HashMap<(String, String), String>>> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));
        values.lock().unwrap().insert(
            ("worker-a".to_owned(), "ROTATED_SECRET".to_owned()),
            "first-value".to_owned(),
        );
        let resolver: Arc<dyn AppSecretResolver> = Arc::new(MutableResolver {
            values: Arc::clone(&values),
        });
        let config = secrets_config(&["ROTATED_SECRET"], &[], 30_000);
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_app_secrets("worker-a".into(), resolver)
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());

        // `allow_env` is false here (deny-all): the private value alone is
        // injected and reaches the guest.
        let res = isolate
            .execute_wasm(sample_secrets_request(), &config)
            .await
            .unwrap();
        let env = parse_env_body(res.body.unwrap().as_ref());
        assert_eq!(
            first_value(&env, "ROTATED_SECRET"),
            Some("first-value"),
            "the initial value must reach the guest"
        );

        // Rotation on the SAME isolate: the next execution sees the NEW value.
        values.lock().unwrap().insert(
            ("worker-a".to_owned(), "ROTATED_SECRET".to_owned()),
            "second-value".to_owned(),
        );
        let res = isolate
            .execute_wasm(sample_secrets_request(), &config)
            .await
            .unwrap();
        let env = parse_env_body(res.body.unwrap().as_ref());
        assert_eq!(
            first_value(&env, "ROTATED_SECRET"),
            Some("second-value"),
            "a rotation must be visible on the next execution of the same isolate"
        );

        // Removal on the SAME isolate: the variable is absent — the old value
        // is never reused.
        values
            .lock()
            .unwrap()
            .remove(&("worker-a".to_owned(), "ROTATED_SECRET".to_owned()));
        let res = isolate
            .execute_wasm(sample_secrets_request(), &config)
            .await
            .unwrap();
        let env = parse_env_body(res.body.unwrap().as_ref());
        assert_eq!(
            first_value(&env, "ROTATED_SECRET"),
            None,
            "a removed secret must be absent on the next execution"
        );
    }

    /// (EDG-52) Secret injection leaves the public config surface intact:
    /// the per-call `WorkerConfig` (env/secrets) is not mutated by the
    /// execution, and neither the `WasiConfig` (derived `Debug` is public)
    /// nor the `WorkerConfig`'s `Debug` ever carries a resolved value.
    #[tokio::test]
    async fn secret_injection_leaves_config_and_debug_intact() {
        // ONE shared admission test lock: this execution enters the async
        // handler and takes a global permit.
        let _lock = super::handler::async_admission_test_lock().await;
        let canary = "cfg-canary-value";
        let resolver: Arc<dyn AppSecretResolver> =
            Arc::new(MapResolver::new(&[("worker-a", "CFG_SECRET", canary)]));
        let config = secrets_config(&["CFG_SECRET"], &[("PUB_ONLY", "public-value")], 30_000);
        let env_before = config.env.clone();
        let secrets_before = config.secrets.clone();
        let mut isolate = WasmIsolate::new(WasiConfig::from_worker_config(&config))
            .with_app_secrets("worker-a".into(), resolver)
            .with_wasm_bytes(wat::parse_str(ENV_DUMP_WAT).unwrap());
        let res = isolate
            .execute_wasm(sample_secrets_request(), &config)
            .await
            .unwrap();
        assert_eq!(res.status, 200);
        // The per-call config is not mutated by the injection.
        assert_eq!(config.env, env_before);
        assert_eq!(config.secrets, secrets_before);
        // No public surface (WasiConfig Debug, WorkerConfig Debug) carries
        // the resolved value.
        let wasi_debug = format!("{:?}", isolate.wasi_config());
        let config_debug = format!("{config:?}");
        assert!(
            !wasi_debug.contains(canary),
            "the WasiConfig Debug must not carry the value"
        );
        assert!(
            !config_debug.contains(canary),
            "the WorkerConfig Debug must not carry the value"
        );
    }

    /// (EDG-52) a READY resolver answer is refused when the shared deadline
    /// already elapsed: tokio's `timeout_at` polls the value future first
    /// and would accept a ready answer even past the deadline, so the
    /// explicit deadline checks (before AND after the await) fail closed.
    /// Deterministic — no sleeps: the deadline is pinned in the past and the
    /// resolver answers immediately.
    #[tokio::test]
    async fn ready_resolution_after_the_deadline_is_refused() {
        let resolver: Arc<dyn AppSecretResolver> = Arc::new(MapResolver::new(&[(
            "worker-a",
            "APP_SECRET",
            "s3cret-value",
        )]));
        let mut isolate =
            WasmIsolate::new(WasiConfig::deny_all()).with_app_secrets("worker-a".into(), resolver);
        let config = secrets_config(&["APP_SECRET"], &[], 30_000);
        let past = std::time::Instant::now() - std::time::Duration::from_millis(1);
        let err = match isolate.resolve_app_secrets(&config, past).await {
            // `WasmSecretEnv` has no `Debug` on purpose: the ok side is
            // asserted without printing it.
            Ok(_) => panic!("a ready answer past the deadline must be refused"),
            Err(err) => err,
        };
        assert_eq!(err.code, "WASM_TIMEOUT");
    }
}
