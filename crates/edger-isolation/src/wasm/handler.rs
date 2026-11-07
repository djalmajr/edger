//! Linear-memory Wasm HTTP ABI.
//!
//! Module exports:
//! - `memory`
//! - `edger_alloc(len: i32) -> i32`
//! - `edger_handle(ptr: i32, len: i32) -> i64`
//! - `_initialize()` — optional; reactor modules (Go `-buildmode=c-shared`)
//!   must run it once per instance before the first ABI call

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use crate::wasm::http::{self, WasmHttpExecution};
use crate::wasm::WasiConfig;
use edger_core::{
    effective_max_body_size_bytes_usize, validate_headers, IsolationError, SerializedRequest,
    SerializedResponse, WorkerConfig,
};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use wasmtime::{
    Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline,
};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

pub(crate) const MAX_REQUEST_FRAME_BYTES: usize = 256 * 1024;
pub(crate) const MAX_RESPONSE_HEADER_BYTES: usize = 64 * 1024;
const DEFAULT_RESPONSE_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODULE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const REQUEST_HEADER_BYTES: usize = 16;
pub(crate) const RESPONSE_HEADER_BYTES: usize = 12;
const DEFAULT_WASM_MEMORY_BYTES: usize = 512 * 1024 * 1024;
const LOW_MEMORY_WASM_MEMORY_BYTES: usize = 128 * 1024 * 1024;
const DEFAULT_WASM_TABLE_ELEMENTS: usize = 16_384;
const DEFAULT_WASM_TIMEOUT_MS: u64 = 30_000;
const WASM_EPOCH_TICK_MS: u64 = 10;

pub struct WasmHttpHandler {
    engine: Engine,
    module_cache: Mutex<HashMap<ModuleCacheKey, Module>>,
    #[cfg(test)]
    in_flight: Arc<AtomicU64>,
    #[cfg(test)]
    peak_in_flight: Arc<AtomicU64>,
}

impl WasmHttpHandler {
    pub fn new() -> Self {
        let engine = build_engine();
        spawn_epoch_ticker(&engine);
        Self {
            engine,
            module_cache: Mutex::new(HashMap::new()),
            #[cfg(test)]
            in_flight: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            peak_in_flight: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn execute_module(
        &self,
        wasm_bytes: &[u8],
        req: &SerializedRequest,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_module_with_config(wasm_bytes, req, &WasiConfig::deny_all())
    }

    pub fn execute_module_with_config(
        &self,
        wasm_bytes: &[u8],
        req: &SerializedRequest,
        wasi: &WasiConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        self.execute_module_with_config_and_limits(
            wasm_bytes,
            req,
            wasi,
            &WasmRuntimeLimits::default(),
        )
    }

    pub(crate) fn execute_module_with_config_and_limits(
        &self,
        wasm_bytes: &[u8],
        req: &SerializedRequest,
        wasi: &WasiConfig,
        limits: &WasmRuntimeLimits,
    ) -> Result<SerializedResponse, IsolationError> {
        let module = self.compiled_module(wasm_bytes)?;

        // (EDG-52 07a) the sync embedding never grants the HTTP import: a
        // module that imports it is refused here (and on a cache hit, even
        // when the module was cached by the async path) with
        // WASM_IMPORT_DENIED — no linker registration, no grant, ever.
        validate_import_policy(&module, /* http_granted */ false)?;

        let mut linker = Linker::<WasmStoreState>::new(&self.engine);
        p1::add_to_linker_sync(&mut linker, |state| &mut state.wasi)
            .map_err(|e| IsolationError::new("WASI_LINK", e.to_string()))?;
        let mut store = Store::new(
            &self.engine,
            WasmStoreState::new(wasi, limits, &WasmSecretEnv::empty()),
        );
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(limits.epoch_deadline_ticks);
        store.epoch_deadline_trap();
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| IsolationError::new("WASM_INSTANTIATE", e.to_string()))?;

        // Reactor modules (Go -buildmode=c-shared) export _initialize and no
        // _start: run it exactly once per instance, on the same store (memory
        // limits and epoch deadline already installed), before the first ABI
        // call. Legacy modules without the export keep the exact old path.
        if instance.get_export(&mut store, "_initialize").is_some() {
            let init = instance
                .get_typed_func::<(), ()>(&mut store, "_initialize")
                .map_err(|e| {
                    IsolationError::new(
                        "WASM_ABI",
                        format!("export _initialize must be a () -> () function: {e}"),
                    )
                })?;
            init.call(&mut store, ()).map_err(|e| {
                IsolationError::new("WASM_EXEC", format!("reactor _initialize failed: {e}"))
            })?;
        }

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| IsolationError::new("WASM_ABI", "memory export missing"))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "edger_alloc")
            .map_err(|e| IsolationError::new("WASM_ABI", format!("export edger_alloc: {e}")))?;
        let handle = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, "edger_handle")
            .map_err(|e| IsolationError::new("WASM_ABI", format!("export edger_handle: {e}")))?;

        let request_frame = encode_request_frame(req)?;
        let request_len = checked_i32_len(request_frame.len(), "request frame")?;
        let request_ptr = alloc
            .call(&mut store, request_len)
            .map_err(|e| IsolationError::new("WASM_EXEC", e.to_string()))?;
        let request_ptr = checked_guest_ptr(request_ptr, "request pointer")?;
        memory
            .write(&mut store, request_ptr, &request_frame)
            .map_err(|e| IsolationError::new("WASM_ABI", format!("request write failed: {e}")))?;

        let packed = handle
            .call(&mut store, (request_ptr as i32, request_len))
            .map_err(|e| IsolationError::new("WASM_EXEC", e.to_string()))?;
        let (response_ptr, response_len) = unpack_ptr_len(packed)?;
        let response_frame_cap = limits.response_frame_bytes();
        if response_len > response_frame_cap {
            return Err(IsolationError::new(
                "WASM_ABI",
                format!("response frame length {response_len} exceeds cap {response_frame_cap}"),
            ));
        }
        let mut response_frame = vec![0u8; response_len];
        memory
            .read(&store, response_ptr, &mut response_frame)
            .map_err(|e| IsolationError::new("WASM_ABI", format!("response read failed: {e}")))?;

        decode_response_frame(&response_frame, limits.response_body_bytes)
    }

    /// Async runtime egress of the same ABI pipeline (EDG-52 05b/06).
    ///
    /// ONE absolute deadline (secret resolution + admission wait + compile
    /// wait + execution) is pinned at the entry of `WasmIsolate::execute_wasm`
    /// and passed in — never reset: the epoch callback yields to the async
    /// executor every 10 ms tick (`Yield(1)` re-arms only the next checkpoint)
    /// and traps once the deadline passes, so yields can never refill the
    /// budget. Linker, instantiation, `_initialize`, alloc/handle and memory
    /// bounds reuse the sync helpers; the import policy is the same (no new
    /// host import, no WASI fs/net/stdio release). The per-execution secret
    /// values reach only this request's fresh Store WASI. A fresh Store per
    /// request; `_initialize` once per instance. ABI/trap errors keep the
    /// `WASM_ABI`/`WASM_EXEC` codes; the total deadline maps to
    /// `WASM_TIMEOUT`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_module_with_config_and_limits_async(
        &self,
        wasm_bytes: &[u8],
        req: &SerializedRequest,
        wasi: &WasiConfig,
        limits: &WasmRuntimeLimits,
        secrets: &WasmSecretEnv,
        deadline: Instant,
        http: Option<&WasmHttpExecution>,
    ) -> Result<SerializedResponse, IsolationError> {
        // (EDG-52) the shared deadline is absolute: if it already elapsed,
        // refuse before taking any permit — no fresh budget starts here.
        if Instant::now() >= deadline {
            return Err(IsolationError::new(
                "WASM_TIMEOUT",
                "wasm total execution budget exhausted",
            ));
        }
        // Admission: the process-wide semaphore bounds the in-flight WASM
        // work (compile wait + active store); the wait is inside the budget.
        let permit = match tokio::time::timeout_at(
            deadline.into(),
            wasm_admission().semaphore().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) | Err(_) => {
                return Err(IsolationError::new(
                    "WASM_TIMEOUT",
                    "wasm admission timed out before the total execution budget elapsed",
                ))
            }
        };
        // A permit that was already available is handed out even after the
        // deadline elapsed (tokio polls the acquire before the timer); the
        // explicit check keeps the budget absolute.
        if Instant::now() >= deadline {
            drop(permit);
            return Err(IsolationError::new(
                "WASM_TIMEOUT",
                "wasm total execution budget exhausted",
            ));
        }

        // (EDG-52 07a) the HTTP import is granted only with BOTH the
        // explicit egress grant and the host transport binding; the flag
        // drives the import-policy re-check on every path (cold compile AND
        // cache hit) with the CURRENT execution's grant — no privilege,
        // linker or Store is ever cached. `http` is `Some` only when both
        // the per-call grant and the binding are present (built in
        // `WasmIsolate::execute_wasm`), so this flag is per-execution.
        let http_granted = http.is_some();

        // Compile: a cache hit is instant; the cold compile (Module::new) is
        // not cancelable and never runs inline on a Tokio thread — the
        // spawn_blocking closure retains the admission permit, which is
        // returned to the execution only after a successful join. Dropping
        // this future mid-compile keeps the permit with the closure until it
        // ends (or fails/panics); a timed-out compile is never claimed to
        // have been cancelled.
        let (module, permit) = self
            .compiled_module_admitted(wasm_bytes, deadline, permit, http_granted)
            .await?;
        // The permit belongs to the execution from here and is released only
        // after the store cleanup (scope exit, below the store in drop
        // order). The test counter drops before the permit so the count
        // never overlaps a permit another request already retook.
        let _admission_permit = permit;
        #[cfg(test)]
        let _in_flight = self.begin_async_request();

        let mut linker = Linker::<WasmStoreState>::new(&self.engine);
        p1::add_to_linker_async(&mut linker, |state| &mut state.wasi)
            .map_err(|e| IsolationError::new("WASI_LINK", e.to_string()))?;
        // (EDG-52 07a) register the versioned HTTP import ONLY for a
        // granted execution. The closure captures the transport `Arc` and
        // the host request id (host metadata, per execution) — never a
        // request, an env or a future: those live only in this execution's
        // linker and fresh Store.
        if let Some(http) = http {
            let transport = Arc::clone(&http.transport);
            let request_id = http.request_id.clone();
            let response_body_bytes = limits.response_body_bytes;
            let response_frame_cap = limits.response_frame_bytes();
            linker
                .func_wrap_async::<_, (i32, i32, i32, i32), Result<i64, wasmtime::Error>>(
                    http::HTTP_IMPORT_MODULE,
                    http::HTTP_IMPORT_FETCH,
                    move |caller, (req_ptr, req_len, out_ptr, out_cap)| {
                        let transport = Arc::clone(&transport);
                        let request_id = request_id.clone();
                        let fut = http::fetch_http(
                            caller,
                            transport,
                            request_id,
                            response_body_bytes,
                            response_frame_cap,
                            deadline,
                            req_ptr,
                            req_len,
                            out_ptr,
                            out_cap,
                        );
                        // `func_wrap_async` wants a boxed (not pinned) host
                        // future; the box owns the `fetch_http` future (and
                        // with it the caller borrow), keeping the same
                        // budget and cancellation semantics.
                        Box::new(fut)
                    },
                )
                .map_err(|e| IsolationError::new("WASM_HTTP_LINK", e.to_string()))?;
        }
        let mut store = Store::new(&self.engine, WasmStoreState::new(wasi, limits, secrets));
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(move |_ctx| {
            if Instant::now() < deadline {
                // Yield to the async executor and re-arm ONLY the next
                // checkpoint: no blocking, no refill of the total budget.
                Ok(UpdateDeadline::Yield(1))
            } else {
                Err(wasmtime::Error::msg(
                    "wasm total execution budget exhausted",
                ))
            }
        });
        let instance = within_budget(
            deadline,
            linker.instantiate_async(&mut store, &module),
            |e| IsolationError::new("WASM_INSTANTIATE", e.to_string()),
        )
        .await?;

        // Reactor modules (Go -buildmode=c-shared) export _initialize and no
        // _start: run it exactly once per instance, on the same store
        // (memory limits and epoch deadline already installed), before the
        // first ABI call. Legacy modules keep the exact old path.
        if instance.get_export(&mut store, "_initialize").is_some() {
            let init = instance
                .get_typed_func::<(), ()>(&mut store, "_initialize")
                .map_err(|e| {
                    IsolationError::new(
                        "WASM_ABI",
                        format!("export _initialize must be a () -> () function: {e}"),
                    )
                })?;
            within_budget(deadline, init.call_async(&mut store, ()), |e| {
                IsolationError::new("WASM_EXEC", format!("reactor _initialize failed: {e}"))
            })
            .await?;
        }

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| IsolationError::new("WASM_ABI", "memory export missing"))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "edger_alloc")
            .map_err(|e| IsolationError::new("WASM_ABI", format!("export edger_alloc: {e}")))?;
        let handle = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, "edger_handle")
            .map_err(|e| IsolationError::new("WASM_ABI", format!("export edger_handle: {e}")))?;

        let request_frame = encode_request_frame(req)?;
        let request_len = checked_i32_len(request_frame.len(), "request frame")?;
        let request_ptr = within_budget(deadline, alloc.call_async(&mut store, request_len), |e| {
            IsolationError::new("WASM_EXEC", e.to_string())
        })
        .await?;
        let request_ptr = checked_guest_ptr(request_ptr, "request pointer")?;
        memory
            .write(&mut store, request_ptr, &request_frame)
            .map_err(|e| IsolationError::new("WASM_ABI", format!("request write failed: {e}")))?;

        let packed = within_budget(
            deadline,
            handle.call_async(&mut store, (request_ptr as i32, request_len)),
            |e| IsolationError::new("WASM_EXEC", e.to_string()),
        )
        .await?;
        let (response_ptr, response_len) = unpack_ptr_len(packed)?;
        let response_frame_cap = limits.response_frame_bytes();
        if response_len > response_frame_cap {
            return Err(IsolationError::new(
                "WASM_ABI",
                format!("response frame length {response_len} exceeds cap {response_frame_cap}"),
            ));
        }
        let mut response_frame = vec![0u8; response_len];
        memory
            .read(&store, response_ptr, &mut response_frame)
            .map_err(|e| IsolationError::new("WASM_ABI", format!("response read failed: {e}")))?;
        let response = decode_response_frame(&response_frame, limits.response_body_bytes)?;
        // (EDG-52) an answer that only completes after the shared deadline
        // already elapsed is not a success: the absolute budget is re-checked
        // before the execution is declared done.
        if Instant::now() >= deadline {
            return Err(IsolationError::new(
                "WASM_TIMEOUT",
                "wasm total execution budget exhausted",
            ));
        }
        Ok(response)
    }

    fn compiled_module(&self, wasm_bytes: &[u8]) -> Result<Module, IsolationError> {
        validate_module_bytes(wasm_bytes)?;
        let key = ModuleCacheKey::from_bytes(wasm_bytes);
        if let Some(module) = self.module_cache()?.get(&key).cloned() {
            return Ok(module);
        }

        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|e| IsolationError::new("WASM_COMPILE", e.to_string()))?;
        // (EDG-52 07a) sync embedding: the HTTP import is never granted.
        validate_import_policy(&module, /* http_granted */ false)?;
        self.module_cache()?.insert(key, module.clone());
        Ok(module)
    }

    /// Compiles the module for an admitted async request. A cache hit returns
    /// the shared module with the caller's permit; a miss compiles on
    /// `spawn_blocking` (see the egress doc) while the closure retains the
    /// admission permit. The cache is the same Arc/Mutex-protected map as the
    /// sync path — no requests, env or futures are ever stored in it.
    /// (EDG-52 07a) the import policy is re-validated on BOTH paths with the
    /// current execution's HTTP grant: a cached module stays denied for a
    /// later execution without grant/binding.
    async fn compiled_module_admitted(
        &self,
        wasm_bytes: &[u8],
        deadline: Instant,
        permit: OwnedSemaphorePermit,
        http_granted: bool,
    ) -> Result<(Module, OwnedSemaphorePermit), IsolationError> {
        validate_module_bytes(wasm_bytes)?;
        let key = ModuleCacheKey::from_bytes(wasm_bytes);
        if let Some(module) = self.module_cache()?.get(&key).cloned() {
            // (EDG-52) a cache hit that lands after the shared deadline
            // elapsed is not a success: a ready value is never accepted past
            // the deadline (tokio polls it before the timer).
            if Instant::now() >= deadline {
                return Err(IsolationError::new(
                    "WASM_TIMEOUT",
                    "wasm total execution budget exhausted",
                ));
            }
            // (EDG-52 07a) cache hit: re-check the import policy with the
            // CURRENT grant (a module cached by a granted execution is
            // denied for one without it).
            validate_import_policy(&module, http_granted)?;
            return Ok((module, permit));
        }
        // The deadline is re-checked before a new cold compile starts: the
        // budget is absolute and never refilled.
        if Instant::now() >= deadline {
            return Err(IsolationError::new(
                "WASM_TIMEOUT",
                "wasm total execution budget exhausted",
            ));
        }

        let engine = self.engine.clone();
        let bytes = wasm_bytes.to_vec();
        #[cfg(test)]
        let fence = compile_fence::capture();
        let join = tokio::task::spawn_blocking(
            move || -> Result<(Module, OwnedSemaphorePermit), IsolationError> {
                #[cfg(test)]
                if let Some(fence) = fence.as_deref() {
                    fence.wait();
                }
                let module = Module::new(&engine, &bytes)
                    .map_err(|e| IsolationError::new("WASM_COMPILE", e.to_string()))?;
                validate_import_policy(&module, http_granted)?;
                Ok((module, permit))
            },
        );

        match tokio::time::timeout_at(deadline.into(), join).await {
            Ok(Ok(result)) => {
                // (EDG-52) a compile result that only becomes ready after
                // the shared deadline elapsed does not extend the budget:
                // the deadline is checked before the result is consumed —
                // `?` must not run ahead of it (a late compile error is a
                // timeout, not WASM_COMPILE).
                if Instant::now() >= deadline {
                    return Err(IsolationError::new(
                        "WASM_TIMEOUT",
                        "wasm total execution budget exhausted",
                    ));
                }
                let (module, permit) = result?;
                self.module_cache()?.insert(key, module.clone());
                Ok((module, permit))
            }
            Ok(Err(join_err)) => {
                // (EDG-52) a join failure that only becomes ready after the
                // deadline is the budget's own exhaustion, not a compile
                // error of this request.
                if Instant::now() >= deadline {
                    return Err(IsolationError::new(
                        "WASM_TIMEOUT",
                        "wasm total execution budget exhausted",
                    ));
                }
                Err(IsolationError::new(
                    "WASM_COMPILE",
                    format!("wasm compile task failed: {join_err:?}"),
                ))
            }
            Err(_) => Err(IsolationError::new(
                "WASM_TIMEOUT",
                "wasm compile timed out before the total execution budget elapsed",
            )),
        }
    }

    fn module_cache(
        &self,
    ) -> Result<MutexGuard<'_, HashMap<ModuleCacheKey, Module>>, IsolationError> {
        self.module_cache
            .lock()
            .map_err(|_| IsolationError::new("WASM_CACHE", "module cache lock poisoned"))
    }

    #[cfg(test)]
    fn cached_module_count(&self) -> Result<usize, IsolationError> {
        Ok(self.module_cache()?.len())
    }

    /// (test-only) counts the async requests admitted by this handler. The
    /// counter increments after the permit is acquired and its guard drops
    /// before the permit guard, so the count never exceeds the held permits.
    #[cfg(test)]
    fn begin_async_request(&self) -> AsyncInFlightGuard {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let mut peak = self.peak_in_flight.load(Ordering::SeqCst);
        loop {
            let current = self.in_flight.load(Ordering::SeqCst);
            if current <= peak {
                break;
            }
            match self.peak_in_flight.compare_exchange_weak(
                peak,
                current,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual) => peak = actual,
            }
        }
        AsyncInFlightGuard {
            counter: Arc::clone(&self.in_flight),
        }
    }

    #[cfg(test)]
    fn in_flight_count(&self) -> u64 {
        self.in_flight.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn peak_in_flight_count(&self) -> u64 {
        self.peak_in_flight.load(Ordering::SeqCst)
    }
}

impl Default for WasmHttpHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ModuleCacheKey([u8; 32]);

impl ModuleCacheKey {
    fn from_bytes(bytes: &[u8]) -> Self {
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        Self(digest)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WasmRuntimeLimits {
    epoch_deadline_ticks: u64,
    memory_bytes: usize,
    response_body_bytes: usize,
    table_elements: usize,
}

impl WasmRuntimeLimits {
    pub(crate) fn from_worker_config(config: &WorkerConfig) -> Self {
        Self {
            epoch_deadline_ticks: epoch_ticks_for_timeout(config.timeout_ms),
            memory_bytes: if config.low_memory {
                LOW_MEMORY_WASM_MEMORY_BYTES
            } else {
                DEFAULT_WASM_MEMORY_BYTES
            },
            response_body_bytes: effective_max_body_size_bytes_usize(config),
            table_elements: DEFAULT_WASM_TABLE_ELEMENTS,
        }
    }

    fn response_frame_bytes(&self) -> usize {
        RESPONSE_HEADER_BYTES
            .saturating_add(MAX_RESPONSE_HEADER_BYTES)
            .saturating_add(self.response_body_bytes)
    }
}

impl Default for WasmRuntimeLimits {
    fn default() -> Self {
        Self {
            epoch_deadline_ticks: epoch_ticks_for_timeout(DEFAULT_WASM_TIMEOUT_MS),
            memory_bytes: DEFAULT_WASM_MEMORY_BYTES,
            response_body_bytes: DEFAULT_RESPONSE_BODY_BYTES,
            table_elements: DEFAULT_WASM_TABLE_ELEMENTS,
        }
    }
}

/// (EDG-52) App-secret values resolved for ONE async execution. Consumed
/// only when building that execution's fresh Store `WasiP1Ctx`, then
/// dropped. Deliberately without `Debug`/`Clone`: the values must not
/// leak through any formatting and are never kept beyond the execution.
pub(crate) struct WasmSecretEnv {
    entries: HashMap<String, String>,
}

impl WasmSecretEnv {
    pub(crate) fn empty() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub(crate) fn insert(&mut self, name: &str, value: String) {
        self.entries.insert(name.to_owned(), value);
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.entries.iter()
    }
}

struct WasmStoreState {
    limits: StoreLimits,
    wasi: WasiP1Ctx,
}

impl WasmStoreState {
    fn new(wasi: &WasiConfig, limits: &WasmRuntimeLimits, secrets: &WasmSecretEnv) -> Self {
        Self {
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.memory_bytes)
                .memories(1)
                .table_elements(limits.table_elements)
                .tables(1)
                .build(),
            wasi: build_wasi_context(wasi, secrets),
        }
    }
}

/// (test-only) RAII decrement of the handler's in-flight async count; the
/// guard is declared after the permit guard, so it drops first and the count
/// never overlaps a permit another request already retook.
#[cfg(test)]
struct AsyncInFlightGuard {
    counter: Arc<AtomicU64>,
}

#[cfg(test)]
impl Drop for AsyncInFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

fn build_engine() -> Engine {
    let mut config = Config::new();
    config.epoch_interruption(true);
    Engine::new(&config).expect("static Wasmtime engine config is valid")
}

fn spawn_epoch_ticker(engine: &Engine) {
    let weak = engine.weak();
    let _epoch_ticker = std::thread::Builder::new()
        .name("edger-wasm-epoch".into())
        .spawn(move || {
            while let Some(engine) = weak.upgrade() {
                std::thread::sleep(Duration::from_millis(WASM_EPOCH_TICK_MS));
                engine.increment_epoch();
            }
        })
        .expect("edger wasm epoch ticker thread starts");
}

fn epoch_ticks_for_timeout(timeout_ms: u64) -> u64 {
    timeout_ms
        .saturating_add(WASM_EPOCH_TICK_MS - 1)
        .checked_div(WASM_EPOCH_TICK_MS)
        .unwrap_or(1)
        .max(1)
}

/// Process-wide admission for the async WASM path: caps the in-flight work
/// (compile wait + active store) at the available parallelism (fallback 1),
/// keeping the execution envelope the sync path got from the Tokio worker
/// threads. It is an internal runtime limit, not a manifest or config field.
struct WasmAdmission {
    semaphore: Arc<Semaphore>,
    #[cfg(test)]
    capacity: usize,
}

impl WasmAdmission {
    /// Clones the semaphore handle so an `acquire_owned` future can own it.
    fn semaphore(&self) -> Arc<Semaphore> {
        Arc::clone(&self.semaphore)
    }

    #[cfg(test)]
    fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.capacity
    }
}

/// (test-only) the process-wide admission's available permits and capacity:
/// lets the isolate-level secret-timeout tests prove that a timed-out
/// resolution holds neither the admission permit nor a Store.
#[cfg(test)]
pub(crate) fn admission_for_test() -> (usize, usize) {
    let admission = wasm_admission();
    (admission.available_permits(), admission.capacity())
}

/// (test-only, shared by `wasm::handler::tests` and `wasm::tests`) serializes
/// every test that acquires an admission permit: the semaphore is
/// process-wide, so a test that asserts on the global permit count must see
/// no other permit holder (a background compile that outlived a timed-out
/// request is reaped by the quiescence wait). ONE static mutex lives here —
/// no second lock, no lock in the product path.
#[cfg(test)]
pub(super) async fn async_admission_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let lock = LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    lock.lock().await
}

fn wasm_admission() -> &'static WasmAdmission {
    static ADMISSION: OnceLock<WasmAdmission> = OnceLock::new();
    ADMISSION.get_or_init(|| {
        let capacity = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(1)
            .max(1);
        WasmAdmission {
            semaphore: Arc::new(Semaphore::new(capacity)),
            #[cfg(test)]
            capacity,
        }
    })
}

/// Total budget of the async path: the admission wait, the compile wait and
/// the guest execution share ONE absolute deadline. It mirrors the sync
/// watchdog helper — the same tick arithmetic converted to a wall duration —
/// so a zero timeout keeps the one-tick (10 ms) watchdog and the default
/// keeps 30 s; the checked/saturating arithmetic never panics on extreme
/// durations.
fn total_execution_budget(limits: &WasmRuntimeLimits) -> Duration {
    Duration::from_millis(
        limits
            .epoch_deadline_ticks
            .saturating_mul(WASM_EPOCH_TICK_MS),
    )
}

/// (EDG-52) The ONE absolute deadline of the whole async budget: the
/// per-execution secret resolution, the admission wait, the compile wait
/// and the guest execution all share it. It is pinned at the entry of
/// `WasmIsolate::execute_wasm` and never reset (the 05b budget): a stalled
/// resolution consumes part of the same budget. `saturating_add` keeps the
/// extreme-timeout semantics overflow-free.
pub(crate) fn execution_deadline(limits: &WasmRuntimeLimits) -> Instant {
    // `checked_add` cannot panic; the (impossible in practice) overflow
    // fails closed — the deadline is `now`, i.e. `WASM_TIMEOUT`.
    Instant::now()
        .checked_add(total_execution_budget(limits))
        .unwrap_or_else(Instant::now)
}

/// Awaits a wasmtime async operation inside the absolute deadline. The
/// deadline is never refilled: once it passes, the wait fails at once and
/// the epoch callback traps the guest. Host-side awaits the epoch callback
/// cannot reach (the guest is suspended inside a host future) are covered
/// here. An error that only becomes READY after the deadline is the
/// budget's own exhaustion (the epoch callback errors exactly then) and is
/// classified `WASM_TIMEOUT`; an error ready inside the deadline keeps the
/// caller's mapping (`tests::within_budget_maps_late_ready_error_to_timeout`
/// and `tests::within_budget_keeps_error_ready_before_deadline`).
async fn within_budget<T>(
    deadline: Instant,
    fut: impl std::future::Future<Output = Result<T, wasmtime::Error>>,
    map_err: impl FnOnce(wasmtime::Error) -> IsolationError,
) -> Result<T, IsolationError> {
    // (EDG-52) the budget is absolute: an already-elapsed deadline is
    // refused before the operation starts.
    if Instant::now() >= deadline {
        return Err(IsolationError::new(
            "WASM_TIMEOUT",
            "wasm total execution budget exhausted",
        ));
    }
    match tokio::time::timeout_at(deadline.into(), fut).await {
        Ok(Ok(value)) => {
            // tokio's `timeout_at` polls the inner future first and accepts a
            // READY answer even after the deadline elapsed; the explicit
            // check keeps a late answer from becoming a success.
            if Instant::now() >= deadline {
                return Err(IsolationError::new(
                    "WASM_TIMEOUT",
                    "wasm total execution budget exhausted",
                ));
            }
            Ok(value)
        }
        Ok(Err(err)) => {
            // (EDG-52) an error that only becomes ready after the deadline
            // is the budget's own exhaustion — the epoch callback errors
            // exactly when the deadline passes — so it is classified
            // WASM_TIMEOUT before ordinary guest errors are mapped. No
            // substring/backtrace sniffing: the absolute deadline decides.
            if Instant::now() >= deadline {
                return Err(IsolationError::new(
                    "WASM_TIMEOUT",
                    "wasm total execution budget exhausted",
                ));
            }
            Err(map_err(err))
        }
        Err(_) => Err(IsolationError::new(
            "WASM_TIMEOUT",
            "wasm total execution budget exhausted",
        )),
    }
}

/// (EDG-52 07a) The guest-import policy of ONE execution. WASIp1 stays the
/// only WASI class; `edger_http_v1::fetch` is admitted ONLY with the
/// current execution's explicit grant + transport binding AND the exact
/// `(i32, i32, i32, i32) -> i64` signature — a different name, module or
/// signature is denied like any other host import (the message carries
/// only the import coordinates, never a payload).
fn validate_import_policy(module: &Module, http_granted: bool) -> Result<(), IsolationError> {
    for import in module.imports() {
        let module_name = import.module();
        let name = import.name();
        if module_name == "wasi_snapshot_preview1" {
            continue;
        }
        if module_name.starts_with("wasi:") {
            return Err(IsolationError::new(
                "WASI_IMPORT_UNSUPPORTED",
                format!("WASI import {module_name}::{name} is not supported for core modules"),
            ));
        }
        if module_name == http::HTTP_IMPORT_MODULE
            && http_granted
            && name == http::HTTP_IMPORT_FETCH
            && http::is_http_fetch_signature(&import.ty())
        {
            continue;
        }
        return Err(IsolationError::new(
            "WASM_IMPORT_DENIED",
            format!("host import {module_name}::{name} is not allowed"),
        ));
    }
    Ok(())
}

fn build_wasi_context(wasi: &WasiConfig, secrets: &WasmSecretEnv) -> WasiP1Ctx {
    let mut builder = WasiCtxBuilder::new();
    builder
        .allow_tcp(wasi.allow_net)
        .allow_udp(wasi.allow_net)
        .allow_ip_name_lookup(wasi.allow_net);
    if wasi.allow_net {
        builder.inherit_network();
    }
    if wasi.allow_stdio {
        builder.inherit_stdout().inherit_stderr();
    }
    // (EDG-52) one entry per name, the secret value winning over a public
    // env of the same name (see `combined_env`); the private values are
    // present even with `allow_env = false`, which grants no fs/net/stdio
    // by itself.
    for (key, value) in combined_env(wasi, secrets) {
        builder.env(key, value);
    }
    builder.build_p1()
}

/// (EDG-52) Combines the public env and the per-execution secret values
/// into the exact entry list handed to the WASI builder: ONE entry per
/// name. `WasiCtxBuilder::env` appends without deduplicating, and a guest
/// `getenv` may read the FIRST entry of a duplicated name — so a secret
/// must not be merely appended after the public env: it replaces a public
/// entry of the same name, and private values are included even when
/// `allow_env` is false (public env is not, and no fs/net/stdio is
/// granted either way). The result is sorted for determinism.
fn combined_env(wasi: &WasiConfig, secrets: &WasmSecretEnv) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = secrets
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if wasi.allow_env {
        for (key, value) in wasi.env.iter() {
            if !entries.iter().any(|(name, _)| name == key) {
                entries.push((key.clone(), value.clone()));
            }
        }
    }
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    entries
}

fn validate_module_bytes(bytes: &[u8]) -> Result<(), IsolationError> {
    if bytes.len() > MAX_MODULE_BYTES {
        return Err(IsolationError::new(
            "WASM_TOO_LARGE",
            format!("module size {} exceeds cap {MAX_MODULE_BYTES}", bytes.len()),
        ));
    }
    const MAGIC: &[u8] = b"\0asm";
    if bytes.len() < 4 || &bytes[..4] != MAGIC {
        return Err(IsolationError::new(
            "WASM_INVALID",
            "missing wasm magic bytes",
        ));
    }
    Ok(())
}

fn encode_request_frame(req: &SerializedRequest) -> Result<Vec<u8>, IsolationError> {
    let headers = serde_json::to_vec(&req.headers).map_err(|e| {
        IsolationError::new(
            "WASM_ABI_ENCODE",
            format!("request headers encode failed: {e}"),
        )
    })?;
    let body = req.body.as_deref().unwrap_or(&[]);
    let method = req.method.as_bytes();
    let uri = req.uri.as_bytes();
    let frame_len = REQUEST_HEADER_BYTES
        .checked_add(method.len())
        .and_then(|len| len.checked_add(uri.len()))
        .and_then(|len| len.checked_add(headers.len()))
        .and_then(|len| len.checked_add(body.len()))
        .ok_or_else(|| IsolationError::new("WASM_ABI_ENCODE", "request frame length overflow"))?;
    if frame_len > MAX_REQUEST_FRAME_BYTES {
        return Err(IsolationError::new(
            "WASM_ABI_ENCODE",
            format!("request frame length {frame_len} exceeds cap {MAX_REQUEST_FRAME_BYTES}"),
        ));
    }
    let mut frame = Vec::with_capacity(frame_len);
    push_u32_len(&mut frame, method.len(), "method")?;
    push_u32_len(&mut frame, uri.len(), "uri")?;
    push_u32_len(&mut frame, headers.len(), "headers")?;
    push_u32_len(&mut frame, body.len(), "body")?;
    frame.extend_from_slice(method);
    frame.extend_from_slice(uri);
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(body);
    Ok(frame)
}

fn decode_response_frame(
    frame: &[u8],
    max_body_bytes: usize,
) -> Result<SerializedResponse, IsolationError> {
    if frame.len() < RESPONSE_HEADER_BYTES {
        return Err(IsolationError::new(
            "WASM_ABI_DECODE",
            "response frame too short",
        ));
    }
    let status = u16::from_le_bytes([frame[0], frame[1]]);
    if !(100..=599).contains(&status) {
        return Err(IsolationError::new(
            "WASM_ABI_DECODE",
            format!("invalid HTTP status {status}"),
        ));
    }
    let headers_len = read_u32_le(frame, 4)? as usize;
    let body_len = read_u32_le(frame, 8)? as usize;
    if headers_len > MAX_RESPONSE_HEADER_BYTES {
        return Err(IsolationError::new(
            "WASM_ABI_DECODE",
            format!("headers length {headers_len} exceeds cap {MAX_RESPONSE_HEADER_BYTES}"),
        ));
    }
    if body_len > max_body_bytes {
        return Err(IsolationError::new(
            "WASM_ABI_DECODE",
            format!("body length {body_len} exceeds cap {max_body_bytes}"),
        ));
    }
    let expected_len = RESPONSE_HEADER_BYTES
        .checked_add(headers_len)
        .and_then(|len| len.checked_add(body_len))
        .ok_or_else(|| IsolationError::new("WASM_ABI_DECODE", "response frame length overflow"))?;
    if frame.len() != expected_len {
        return Err(IsolationError::new(
            "WASM_ABI_DECODE",
            format!(
                "response frame length {} does not match encoded length {expected_len}",
                frame.len()
            ),
        ));
    }
    let headers_start = RESPONSE_HEADER_BYTES;
    let body_start = headers_start + headers_len;
    let headers: Vec<(String, String)> = serde_json::from_slice(&frame[headers_start..body_start])
        .map_err(|e| {
            IsolationError::new(
                "WASM_ABI_DECODE",
                format!("response headers decode failed: {e}"),
            )
        })?;
    validate_headers(&headers).map_err(|e| IsolationError::new(&e.code, e.message))?;
    let body = if body_len == 0 {
        None
    } else {
        Some(bytes::Bytes::copy_from_slice(&frame[body_start..]))
    };
    Ok(SerializedResponse {
        status,
        headers,
        body,
    })
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, IsolationError> {
    let end = offset + 4;
    let slice = bytes.get(offset..end).ok_or_else(|| {
        IsolationError::new("WASM_ABI_DECODE", "u32 read out of response frame bounds")
    })?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn push_u32_len(frame: &mut Vec<u8>, len: usize, label: &str) -> Result<(), IsolationError> {
    let len = u32::try_from(len).map_err(|_| {
        IsolationError::new(
            "WASM_ABI_ENCODE",
            format!("{label} length exceeds u32 range"),
        )
    })?;
    frame.extend_from_slice(&len.to_le_bytes());
    Ok(())
}

fn checked_i32_len(len: usize, label: &str) -> Result<i32, IsolationError> {
    i32::try_from(len)
        .map_err(|_| IsolationError::new("WASM_ABI", format!("{label} exceeds i32 ABI range")))
}

fn checked_guest_ptr(ptr: i32, label: &str) -> Result<usize, IsolationError> {
    usize::try_from(ptr)
        .map_err(|_| IsolationError::new("WASM_ABI", format!("{label} must be non-negative")))
}

fn unpack_ptr_len(value: i64) -> Result<(usize, usize), IsolationError> {
    let bits = value as u64;
    let ptr = (bits & 0xffff_ffff) as u32;
    let len = (bits >> 32) as u32;
    let len = usize::try_from(len)
        .map_err(|_| IsolationError::new("WASM_ABI", "response length exceeds usize range"))?;
    Ok((ptr as usize, len))
}

/// (test-only) deterministic gate on the async compile path: the
/// spawn_blocking closure waits at the gate BEFORE the real `Module::new` —
/// the compiler is never replaced by a fake return. The RAII guard plus the
/// stall timeout guarantee the worker thread is never left parked, even if
/// the test panics mid-assertion.
#[cfg(test)]
mod compile_fence {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    pub(super) struct Fence {
        entered: AtomicBool,
        released: AtomicBool,
    }

    pub(super) struct Guard(Arc<Fence>);

    impl Fence {
        pub(super) fn new() -> Self {
            Self {
                entered: AtomicBool::new(false),
                released: AtomicBool::new(false),
            }
        }

        pub(super) fn entered(&self) -> bool {
            self.entered.load(Ordering::SeqCst)
        }

        pub(super) fn release(&self) {
            self.released.store(true, Ordering::SeqCst);
        }

        /// Marks the compile entry and parks until the guard releases (or
        /// the stall limit is hit): the real compiler always runs.
        pub(super) fn wait(&self) {
            self.entered.store(true, Ordering::SeqCst);
            const STALL_LIMIT: Duration = Duration::from_secs(30);
            let started = Instant::now();
            while !self.released.load(Ordering::SeqCst) {
                if started.elapsed() >= STALL_LIMIT {
                    break; // never leave a blocking worker parked
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    thread_local! {
        static FENCE: std::cell::RefCell<Option<Arc<Fence>>> = const { std::cell::RefCell::new(None) };
    }

    /// Installs the gate on this thread for the calls it makes.
    pub(super) fn install(fence: Arc<Fence>) -> Guard {
        FENCE.with(|slot| *slot.borrow_mut() = Some(fence.clone()));
        Guard(fence)
    }

    /// Captures the gate (if any) so the compile closure can wait at it.
    pub(super) fn capture() -> Option<Arc<Fence>> {
        FENCE.with(|slot| slot.borrow().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    use crate::wasm::WasmIsolate;
    use edger_core::{parse_worker_config, Isolate, WorkerManifest};

    const ECHO_URI_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (data (i32.const 512) "[[\"content-type\",\"text/plain\"],[\"x-wasm-abi\",\"v2\"]]")
          (data (i32.const 600) "wasm path: ")
          (global $heap (mut i32) (i32.const 1024))

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
            (local $method_len i32)
            (local $uri_len i32)
            (local $uri_ptr i32)
            (local $body_ptr i32)
            (local $body_len i32)
            (local $frame_len i32)

            local.get $req_ptr
            i32.load
            local.set $method_len
            local.get $req_ptr
            i32.const 4
            i32.add
            i32.load
            local.set $uri_len
            local.get $req_ptr
            i32.const 16
            i32.add
            local.get $method_len
            i32.add
            local.set $uri_ptr

            i32.const 4159
            local.set $body_ptr
            i32.const 11
            local.get $uri_len
            i32.add
            local.set $body_len
            i32.const 74
            local.get $uri_len
            i32.add
            local.set $frame_len

            i32.const 4096
            i32.const 200
            i32.store16
            i32.const 4100
            i32.const 51
            i32.store
            i32.const 4104
            local.get $body_len
            i32.store
            i32.const 4108
            i32.const 512
            i32.const 51
            call $copy
            local.get $body_ptr
            i32.const 600
            i32.const 11
            call $copy
            local.get $body_ptr
            i32.const 11
            i32.add
            local.get $uri_ptr
            local.get $uri_len
            call $copy

            i32.const 4096
            i64.extend_i32_u
            local.get $frame_len
            i64.extend_i32_u
            i64.const 32
            i64.shl
            i64.or
          )
        )
    "#;

    // Reactor fixture (Go -buildmode=c-shared shape): `_initialize` bumps a
    // per-instance counter; `edger_handle` answers 500 until the counter is
    // above 0, then a 200 whose body records the counter. alloc/handle only
    // work correctly once `_initialize` ran on this instance.
    const REACTOR_INIT_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (global $init_count (mut i32) (i32.const 0))
          (global $heap (mut i32) (i32.const 1024))
          (data (i32.const 512) "[[\"content-type\",\"text/plain\"]]")
          (data (i32.const 600) "init count ")

          (func (export "_initialize")
            (global.get $init_count)
            (i32.const 1)
            (i32.add)
            (global.set $init_count)
          )

          (func (export "edger_alloc") (param $len i32) (result i32)
            (local $ptr i32)
            (global.get $init_count)
            (i32.const 1)
            (i32.ne)
            if
              unreachable
            end
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
            (local $count i32)
            global.get $init_count
            local.set $count

            local.get $count
            i32.eqz
            if
              i32.const 4096
              i32.const 500
              i32.store16
              i32.const 4100
              i32.const 31
              i32.store
              i32.const 4104
              i32.const 0
              i32.store
              i32.const 4108
              i32.const 512
              i32.const 31
              call $copy

              i32.const 4096
              i64.extend_i32_u
              i64.const 43
              i64.const 32
              i64.shl
              i64.or
              return
            end

            i32.const 611
            i32.const 48
            local.get $count
            i32.add
            i32.store8

            i32.const 4096
            i32.const 200
            i32.store16
            i32.const 4100
            i32.const 31
            i32.store
            i32.const 4104
            i32.const 12
            i32.store
            i32.const 4108
            i32.const 512
            i32.const 31
            call $copy
            i32.const 4139
            i32.const 600
            i32.const 12
            call $copy

            i32.const 4096
            i64.extend_i32_u
            i64.const 55
            i64.const 32
            i64.shl
            i64.or
          )
        )
    "#;

    #[test]
    fn executes_request_response_abi() {
        let wasm_bytes = wat::parse_str(ECHO_URI_WAT).unwrap();
        let handler = WasmHttpHandler::new();
        let req = SerializedRequest {
            method: "POST".into(),
            uri: "/from-handler-test".into(),
            headers: vec![("x-proof".into(), "present".into())],
            body: Some(bytes::Bytes::from_static(b"payload")),
            request_id: "wasm-unit".into(),
            base_href: None,
        };
        let res = handler.execute_module(&wasm_bytes, &req).unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(
            res.headers,
            vec![
                ("content-type".into(), "text/plain".into()),
                ("x-wasm-abi".into(), "v2".into())
            ]
        );
        assert_eq!(
            res.body.as_ref().map(|b| b.as_ref()),
            Some(b"wasm path: /from-handler-test".as_ref())
        );
    }

    #[test]
    fn repeated_requests_reuse_cached_module() {
        let wasm_bytes = wat::parse_str(ECHO_URI_WAT).unwrap();
        let handler = WasmHttpHandler::new();
        let first = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap();
        let second = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap();

        assert_eq!(first.status, 200);
        assert_eq!(second.status, 200);
        assert_eq!(handler.cached_module_count().unwrap(), 1);
    }

    #[test]
    fn epoch_watchdog_interrupts_infinite_loop() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "edger_alloc") (param $len i32) (result i32)
                i32.const 1024
              )
              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
                loop $forever
                  br $forever
                end
                i64.const 0
              )
            )
            "#,
        )
        .unwrap();
        let handler = WasmHttpHandler::new();
        let limits = WasmRuntimeLimits {
            epoch_deadline_ticks: 1,
            ..WasmRuntimeLimits::default()
        };
        let started = std::time::Instant::now();
        let err = handler
            .execute_module_with_config_and_limits(
                &wasm_bytes,
                &sample_request(),
                &WasiConfig::deny_all(),
                &limits,
            )
            .unwrap_err();

        assert_eq!(err.code, "WASM_EXEC");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn store_limits_reject_memory_above_budget() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 2)
              (func (export "edger_alloc") (param $len i32) (result i32)
                i32.const 1024
              )
              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
                i64.const 0
              )
            )
            "#,
        )
        .unwrap();
        let handler = WasmHttpHandler::new();
        let limits = WasmRuntimeLimits {
            memory_bytes: 64 * 1024,
            ..WasmRuntimeLimits::default()
        };
        let err = handler
            .execute_module_with_config_and_limits(
                &wasm_bytes,
                &sample_request(),
                &WasiConfig::deny_all(),
                &limits,
            )
            .unwrap_err();

        assert_eq!(err.code, "WASM_INSTANTIATE");
    }

    #[test]
    fn store_limits_reject_multiple_memories() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (memory 1)
              (func (export "edger_alloc") (param $len i32) (result i32) i32.const 1024)
              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64) i64.const 0)
            )
            "#,
        )
        .unwrap();
        let err = WasmHttpHandler::new()
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap_err();

        assert_eq!(err.code, "WASM_INSTANTIATE");
    }

    #[test]
    fn store_limits_reject_multiple_tables() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (table 1 funcref)
              (table 1 funcref)
              (func (export "edger_alloc") (param $len i32) (result i32) i32.const 1024)
              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64) i64.const 0)
            )
            "#,
        )
        .unwrap();
        let err = WasmHttpHandler::new()
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap_err();

        assert_eq!(err.code, "WASM_INSTANTIATE");
    }

    #[test]
    fn store_limits_reject_table_above_element_cap() {
        // Real WAT module whose single funcref table minimum is one element
        // above the cap: the store limiter must reject it at instantiation
        // (not a constant comparison).
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (table 16385 funcref)
              (func (export "edger_alloc") (param $len i32) (result i32) i32.const 1024)
              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64) i64.const 0)
            )
            "#,
        )
        .unwrap();
        let err = WasmHttpHandler::new()
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap_err();

        assert_eq!(err.code, "WASM_INSTANTIATE");
    }

    #[test]
    fn returns_response_body_larger_than_64_kib() {
        const LARGE_BODY_LEN: usize = 70 * 1024;
        let frame_len = RESPONSE_HEADER_BYTES + 2 + LARGE_BODY_LEN;
        let wasm = format!(
            r#"
            (module
              (memory (export "memory") 2)
              (data (i32.const 1024) "[]")

              (func (export "edger_alloc") (param $len i32) (result i32)
                i32.const 2048
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

              (func $fill (param $dst i32) (param $len i32)
                (local $i i32)
                loop $fill_loop
                  local.get $i
                  local.get $len
                  i32.lt_u
                  if
                    local.get $dst
                    local.get $i
                    i32.add
                    i32.const 65
                    i32.store8
                    local.get $i
                    i32.const 1
                    i32.add
                    local.set $i
                    br $fill_loop
                  end
                end
              )

              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
                i32.const 4096
                i32.const 200
                i32.store16
                i32.const 4100
                i32.const 2
                i32.store
                i32.const 4104
                i32.const {LARGE_BODY_LEN}
                i32.store
                i32.const 4108
                i32.const 1024
                i32.const 2
                call $copy
                i32.const 4110
                i32.const {LARGE_BODY_LEN}
                call $fill

                i32.const 4096
                i64.extend_i32_u
                i64.const {frame_len}
                i64.const 32
                i64.shl
                i64.or
              )
            )
            "#
        );
        let wasm_bytes = wat::parse_str(&wasm).unwrap();
        let handler = WasmHttpHandler::new();
        let res = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap();
        let body = res.body.unwrap();

        assert_eq!(res.status, 200);
        assert_eq!(body.len(), LARGE_BODY_LEN);
        assert!(body.iter().all(|byte| *byte == b'A'));
    }

    #[test]
    fn rejects_invalid_wasm_magic() {
        let handler = WasmHttpHandler::new();
        let err = handler
            .execute_module(b"not-wasm", &sample_request())
            .unwrap_err();
        assert_eq!(err.code, "WASM_INVALID");
    }

    #[test]
    fn rejects_oversized_wasm_module() {
        let mut wasm_bytes = b"\0asm".to_vec();
        wasm_bytes.resize(MAX_MODULE_BYTES + 1, 0);

        let handler = WasmHttpHandler::new();
        let err = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap_err();

        assert_eq!(err.code, "WASM_TOO_LARGE");
    }

    #[test]
    fn rejects_host_imports() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (import "env" "host_call" (func $host_call))
            )
            "#,
        )
        .unwrap();

        let handler = WasmHttpHandler::new();
        let err = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap_err();

        assert_eq!(err.code, "WASM_IMPORT_DENIED");
    }

    #[test]
    fn links_wasi_preview1_imports_with_deny_all_context() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (import "wasi_snapshot_preview1" "fd_write"
                (func $fd_write (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (data (i32.const 64) "[]")
              (data (i32.const 96) "wasi ok")
              (data (i32.const 128) "log")
              (global $heap (mut i32) (i32.const 1024))

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
                i32.const 160
                i32.const 128
                i32.store
                i32.const 164
                i32.const 3
                i32.store
                i32.const 1
                i32.const 160
                i32.const 1
                i32.const 168
                call $fd_write
                drop

                i32.const 512
                i32.const 200
                i32.store16
                i32.const 516
                i32.const 2
                i32.store
                i32.const 520
                i32.const 7
                i32.store
                i32.const 524
                i32.const 64
                i32.const 2
                call $copy
                i32.const 526
                i32.const 96
                i32.const 7
                call $copy

                i32.const 512
                i64.extend_i32_u
                i64.const 21
                i64.const 32
                i64.shl
                i64.or
              )
            )
            "#,
        )
        .unwrap();

        let handler = WasmHttpHandler::new();
        let res = handler
            .execute_module_with_config(&wasm_bytes, &sample_request(), &WasiConfig::deny_all())
            .unwrap();

        assert_eq!(res.status, 200);
        assert_eq!(
            res.body.as_ref().map(|b| b.as_ref()),
            Some(b"wasi ok".as_ref())
        );
    }

    #[test]
    fn reactor_initialize_runs_before_first_abi_call() {
        let wasm_bytes = wat::parse_str(REACTOR_INIT_WAT).unwrap();
        let handler = WasmHttpHandler::new();
        let res = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap();

        assert_eq!(res.status, 200);
        assert_eq!(
            res.headers,
            vec![("content-type".into(), "text/plain".into())]
        );
        assert_eq!(
            res.body.as_ref().map(|b| b.as_ref()),
            Some(b"init count 1".as_ref())
        );
    }

    #[test]
    fn repeated_requests_run_initialize_once_per_store() {
        let wasm_bytes = wat::parse_str(REACTOR_INIT_WAT).unwrap();
        let handler = WasmHttpHandler::new();
        let first = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap();
        let second = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap();

        assert_eq!(
            first.body.as_ref().map(|b| b.as_ref()),
            Some(b"init count 1".as_ref())
        );
        assert_eq!(
            second.body.as_ref().map(|b| b.as_ref()),
            Some(b"init count 1".as_ref())
        );
        assert_eq!(handler.cached_module_count().unwrap(), 1);
    }

    #[test]
    fn non_function_initialize_export_is_rejected() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (global (export "_initialize") (mut i32) (i32.const 7))
              (func (export "edger_alloc") (param $len i32) (result i32) i32.const 1024)
              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
                i32.const 4096
                i32.const 200
                i32.store16
                i32.const 4100
                i32.const 0
                i32.store
                i32.const 4104
                i32.const 0
                i32.store
                i32.const 4096
                i64.extend_i32_u
                i64.const 12
                i64.const 32
                i64.shl
                i64.or
              )
            )
            "#,
        )
        .unwrap();
        let handler = WasmHttpHandler::new();
        let err = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap_err();

        assert_eq!(err.code, "WASM_ABI");
    }

    #[test]
    fn wrong_signature_initialize_export_is_rejected() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "_initialize") (param i32))
              (func (export "edger_alloc") (param $len i32) (result i32) i32.const 1024)
              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
                i64.const 0
              )
            )
            "#,
        )
        .unwrap();
        let handler = WasmHttpHandler::new();
        let err = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap_err();

        assert_eq!(err.code, "WASM_ABI");
    }

    #[test]
    fn trapped_initialize_never_reaches_handler() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "_initialize") (unreachable))
              (func (export "edger_alloc") (param $len i32) (result i32) i32.const 1024)
              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
                i64.const 0
              )
            )
            "#,
        )
        .unwrap();
        let handler = WasmHttpHandler::new();
        let err = handler
            .execute_module(&wasm_bytes, &sample_request())
            .unwrap_err();

        assert_eq!(err.code, "WASM_EXEC");
        assert!(err.message.contains("_initialize"));
    }

    #[test]
    fn epoch_budget_interrupts_infinite_initialize() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (memory (export "memory") 1)
              (func (export "_initialize")
                loop $forever
                  br $forever
                end
              )
              (func (export "edger_alloc") (param $len i32) (result i32) i32.const 1024)
              (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
                i64.const 0
              )
            )
            "#,
        )
        .unwrap();
        let handler = WasmHttpHandler::new();
        let limits = WasmRuntimeLimits {
            epoch_deadline_ticks: 1,
            ..WasmRuntimeLimits::default()
        };
        let started = std::time::Instant::now();
        let err = handler
            .execute_module_with_config_and_limits(
                &wasm_bytes,
                &sample_request(),
                &WasiConfig::deny_all(),
                &limits,
            )
            .unwrap_err();

        assert_eq!(err.code, "WASM_EXEC");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    // CPU-spin fixture (EDG-52 async): `edger_handle` reads the WASI
    // MONOTONIC clock (the only import class the policy allows) until 150 ms
    // of guest CPU have elapsed, then answers the usual v2 frame with the
    // fixed body. Each loop iteration is a real ABI call, so the guest makes
    // progress; the wall spin keeps the test machine-speed independent.
    const CPU_SPIN_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "clock_time_get"
            (func $clock (param i32 i64 i32) (result i32)))
          (memory (export "memory") 2)
          (data (i32.const 512) "[[\"content-type\",\"text/plain\"],[\"x-wasm-abi\",\"v2\"]]")
          (data (i32.const 600) "cpu done")
          (global $heap (mut i32) (i32.const 1024))

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

          ;; MONOTONIC clock (id 1); the ns timestamp lands in guest memory at 32.
          (func $read_ns (result i64)
            i32.const 1
            i64.const 0
            i32.const 32
            call $clock
            drop
            i32.const 32
            i64.load
          )

          (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
            (local $start i64)
            (local $now i64)
            (call $read_ns)
            local.set $start
            (block $done
              (loop $spin
                (call $read_ns)
                local.set $now
                local.get $now
                local.get $start
                i64.sub
                ;; 150 ms = 150_000_000 ns
                i64.const 150000000
                i64.lt_s
                if
                  br $spin
                end
                br $done
              )
            )

            i32.const 4096
            i32.const 200
            i32.store16
            i32.const 4100
            i32.const 51
            i32.store
            i32.const 4104
            i32.const 8
            i32.store
            i32.const 4108
            i32.const 512
            i32.const 51
            call $copy
            i32.const 4159
            i32.const 600
            i32.const 8
            call $copy

            i32.const 4096
            i64.extend_i32_u
            i64.const 71
            i64.const 32
            i64.shl
            i64.or
          )
        )
    "#;

    fn test_config(timeout_ms: u64) -> WorkerConfig {
        let mut config = parse_worker_config(&WorkerManifest {
            name: "wasm-async".into(),
            kind: Some("wasm".into()),
            ..Default::default()
        });
        config.timeout_ms = timeout_ms;
        config
    }

    #[tokio::test(flavor = "current_thread")]
    async fn guest_cpu_keeps_tokio_heartbeat_progressing() {
        let _lock = async_admission_test_lock().await;
        let wasm_bytes = wat::parse_str(CPU_SPIN_WAT).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(wasm_bytes);
        let req = SerializedRequest {
            method: "GET".into(),
            uri: "/cpu-spin".into(),
            headers: vec![],
            body: None,
            request_id: "wasm-async".into(),
            base_href: None,
        };
        let config = test_config(5_000);

        let ticks = Arc::new(AtomicU64::new(0));
        let heartbeat = {
            let counter = Arc::clone(&ticks);
            tokio::spawn(async move {
                loop {
                    counter.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
        };

        let started = Instant::now();
        let res = isolate.execute_wasm(req, &config).await.unwrap();
        let wall = started.elapsed();
        heartbeat.abort(); // this task only, by its recorded handle
        let hbeats = ticks.load(Ordering::SeqCst);

        assert_eq!(res.status, 200);
        assert_eq!(
            res.body.as_ref().map(|b| b.as_ref()),
            Some(b"cpu done".as_ref())
        );
        assert!(
            wall >= Duration::from_millis(150),
            "guest ran the full spin: {wall:?}"
        );
        assert!(
            wall < Duration::from_secs(3),
            "guest finished well inside the timeout: {wall:?}"
        );
        assert!(
            hbeats >= 5,
            "heartbeat progressed while the guest executed CPU: {hbeats} ticks in {wall:?}"
        );
    }

    // The spin loop only ends when the real clock advances: a guest stuck in
    // a yield storm without progress would hit the budget instead of the 200
    // above, so the response proves progress across yields.

    // Infinite-loop fixtures: budget must cut them (and never refill).
    const INFINITE_HANDLE_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "edger_alloc") (param $len i32) (result i32)
            i32.const 1024
          )
          (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
            loop $forever
              br $forever
            end
            i64.const 0
          )
        )
    "#;

    const INFINITE_INIT_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "_initialize")
            loop $forever
              br $forever
            end
          )
          (func (export "edger_alloc") (param $len i32) (result i32) i32.const 1024)
          (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
            i64.const 0
          )
        )
    "#;

    /// (EDG-52 P2) the START section loops: instantiation never finishes
    /// inside the budget, so the deadline must surface as WASM_TIMEOUT from
    /// the instantiation path, not as WASM_INSTANTIATE.
    const INFINITE_START_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func $spin
            loop $forever
              br $forever
            end
          )
          (start $spin)
          (func (export "edger_alloc") (param $len i32) (result i32) i32.const 1024)
          (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
            i64.const 0
          )
        )
    "#;

    // 5 s spin: long enough that a cancelled request is still in the guest
    // when the test aborts it (a working cancel ends it far earlier).
    const LONG_SPIN_WAT: &str = r#"
        (module
          (import "wasi_snapshot_preview1" "clock_time_get"
            (func $clock (param i32 i64 i32) (result i32)))
          (memory (export "memory") 2)
          (data (i32.const 512) "[[\"content-type\",\"text/plain\"]]" )
          (data (i32.const 600) "long spin done")
          (global $heap (mut i32) (i32.const 1024))

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

          (func $read_ns (result i64)
            i32.const 1
            i64.const 0
            i32.const 32
            call $clock
            drop
            i32.const 32
            i64.load
          )

          (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
            (local $start i64)
            (local $now i64)
            (call $read_ns)
            local.set $start
            (block $done
              (loop $spin
                (call $read_ns)
                local.set $now
                local.get $now
                local.get $start
                i64.sub
                ;; 5 s = 5_000_000_000 ns
                i64.const 5000000000
                i64.lt_s
                if
                  br $spin
                end
                br $done
              )
            )

            i32.const 4096
            i32.const 200
            i32.store16
            i32.const 4100
            i32.const 31
            i32.store
            i32.const 4104
            i32.const 14
            i32.store
            i32.const 4108
            i32.const 512
            i32.const 31
            call $copy
            i32.const 4159
            i32.const 600
            i32.const 14
            call $copy

            i32.const 4096
            i64.extend_i32_u
            i64.const 57
            i64.const 32
            i64.shl
            i64.or
          )
        )
    "#;

    /// The absolute deadline surfaces as WASM_TIMEOUT — and only it:
    /// WASM_EXEC/WASM_INSTANTIATE are genuine trap classifications and are
    /// NOT proof of a budget cut (EDG-52 P2 — a deadline exhausted through
    /// the epoch callback used to surface as WASM_EXEC and these tests had
    /// to catch it here).
    fn is_budget_cut(code: &str) -> bool {
        matches!(code, "WASM_TIMEOUT")
    }

    /// (test-only) waits until the process-wide admission is quiescent.
    async fn async_admission_quiescent() {
        let admission = wasm_admission();
        let deadline = Instant::now() + Duration::from_secs(10);
        while admission.available_permits() < admission.capacity() {
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    #[test]
    fn total_budget_mirrors_watchdog_tick_semantics() {
        // zero timeout keeps the one-tick watchdog, as in the sync helper
        assert_eq!(epoch_ticks_for_timeout(0), 1);
        assert_eq!(epoch_ticks_for_timeout(10), 1);
        assert_eq!(epoch_ticks_for_timeout(30_000), 3_000);
        let one_tick = WasmRuntimeLimits {
            epoch_deadline_ticks: 1,
            ..Default::default()
        };
        assert_eq!(total_execution_budget(&one_tick), Duration::from_millis(10));
        assert_eq!(
            total_execution_budget(&WasmRuntimeLimits::default()),
            Duration::from_millis(30_000)
        );
        // extreme durations stay panic-free (checked/saturating arithmetic)
        let extreme = WasmRuntimeLimits {
            epoch_deadline_ticks: u64::MAX,
            ..Default::default()
        };
        assert!(total_execution_budget(&extreme) > Duration::ZERO);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_path_preserves_abi_frame_headers_and_bounds() {
        let _lock = async_admission_test_lock().await;
        let wasm_bytes = wat::parse_str(ECHO_URI_WAT).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(wasm_bytes);
        let req = SerializedRequest {
            method: "POST".into(),
            uri: "/from-async-handler-test".into(),
            headers: vec![("x-proof".into(), "present".into())],
            body: Some(bytes::Bytes::from_static(b"payload")),
            request_id: "wasm-async".into(),
            base_href: None,
        };
        let res = isolate
            .execute_wasm(req, &test_config(30_000))
            .await
            .unwrap();

        assert_eq!(res.status, 200);
        assert_eq!(
            res.headers,
            vec![
                ("content-type".into(), "text/plain".into()),
                ("x-wasm-abi".into(), "v2".into())
            ]
        );
        assert_eq!(
            res.body.as_ref().map(|b| b.as_ref()),
            Some(b"wasm path: /from-async-handler-test".as_ref())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_repeated_requests_run_initialize_once_per_store() {
        let _lock = async_admission_test_lock().await;
        let wasm_bytes = wat::parse_str(REACTOR_INIT_WAT).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(wasm_bytes);
        let first = isolate
            .execute_wasm(sample_request(), &test_config(30_000))
            .await
            .unwrap();
        let second = isolate
            .execute_wasm(sample_request(), &test_config(30_000))
            .await
            .unwrap();

        // Fresh store per request: each execution re-runs _initialize on its
        // own instance (counter back to 1), while the module is shared.
        assert_eq!(
            first.body.as_ref().map(|b| b.as_ref()),
            Some(b"init count 1".as_ref())
        );
        assert_eq!(
            second.body.as_ref().map(|b| b.as_ref()),
            Some(b"init count 1".as_ref())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_path_keeps_import_policy_and_validation_errors() {
        let _lock = async_admission_test_lock().await;
        let denied =
            wat::parse_str(r#"(module (import "env" "host_call" (func $host_call)))"#).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(denied);
        let err = isolate
            .execute_wasm(sample_request(), &test_config(30_000))
            .await
            .unwrap_err();
        assert_eq!(err.code, "WASM_IMPORT_DENIED");

        let mut isolate =
            WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(b"not-wasm".to_vec());
        let err = isolate
            .execute_wasm(sample_request(), &test_config(30_000))
            .await
            .unwrap_err();
        assert_eq!(err.code, "WASM_INVALID");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_budget_cuts_infinite_handle_loop_without_refill() {
        let _lock = async_admission_test_lock().await;
        let wasm_bytes = wat::parse_str(INFINITE_HANDLE_WAT).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(wasm_bytes);
        let started = Instant::now();
        let err = isolate
            .execute_wasm(sample_request(), &test_config(100))
            .await
            .unwrap_err();
        let wall = started.elapsed();

        assert!(is_budget_cut(&err.code), "budget cut, got: {}", err.code);
        // A budget refilled by yields would keep the loop past the window.
        assert!(
            wall < Duration::from_secs(1),
            "budget was not refilled by yields: {wall:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_budget_cuts_infinite_initialize() {
        let _lock = async_admission_test_lock().await;
        let wasm_bytes = wat::parse_str(INFINITE_INIT_WAT).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(wasm_bytes);
        let started = Instant::now();
        let err = isolate
            .execute_wasm(sample_request(), &test_config(100))
            .await
            .unwrap_err();
        let wall = started.elapsed();

        assert!(is_budget_cut(&err.code), "budget cut, got: {}", err.code);
        assert!(
            wall < Duration::from_secs(1),
            "budget was not refilled by yields: {wall:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_budget_cuts_infinite_start_section() {
        let _lock = async_admission_test_lock().await;
        let wasm_bytes = wat::parse_str(INFINITE_START_WAT).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(wasm_bytes);
        let started = Instant::now();
        let err = isolate
            .execute_wasm(sample_request(), &test_config(100))
            .await
            .unwrap_err();
        let wall = started.elapsed();

        assert!(is_budget_cut(&err.code), "budget cut, got: {}", err.code);
        assert!(
            wall < Duration::from_secs(1),
            "budget was not refilled by yields: {wall:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_zero_timeout_keeps_one_tick_watchdog() {
        let _lock = async_admission_test_lock().await;
        let wasm_bytes = wat::parse_str(INFINITE_HANDLE_WAT).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(wasm_bytes);
        let started = Instant::now();
        let err = isolate
            .execute_wasm(sample_request(), &test_config(0))
            .await
            .unwrap_err();
        let wall = started.elapsed();

        assert!(
            is_budget_cut(&err.code),
            "zero timeout still watches, got: {}",
            err.code
        );
        assert!(
            wall < Duration::from_millis(1_000),
            "one-tick budget: {wall:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_during_guest_releases_admission_and_store() {
        let _lock = async_admission_test_lock().await;
        async_admission_quiescent().await;
        let handler = Arc::new(WasmHttpHandler::new());
        let wasm_bytes = wat::parse_str(LONG_SPIN_WAT).unwrap();
        let admission = wasm_admission();
        let capacity = admission.capacity();

        let fence = Arc::new(compile_fence::Fence::new());
        let _gate = compile_fence::install(fence.clone());

        let task = {
            let handler = Arc::clone(&handler);
            let req = sample_request();
            let limits = WasmRuntimeLimits::default();
            tokio::spawn(async move {
                handler
                    .execute_module_with_config_and_limits_async(
                        &wasm_bytes,
                        &req,
                        &WasiConfig::deny_all(),
                        &limits,
                        &WasmSecretEnv::empty(),
                        execution_deadline(&limits),
                        None,
                    )
                    .await
            })
        };

        // The cold compile reaches the gate, then the real compiler runs and
        // the guest starts its 5 s spin.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fence.entered() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(fence.entered(), "cold compile reached the gate");
        fence.release();

        let deadline = Instant::now() + Duration::from_secs(10);
        while handler.in_flight_count() == 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            handler.in_flight_count(),
            1,
            "the async request is in flight"
        );
        assert_eq!(
            admission.available_permits(),
            capacity - 1,
            "the guest holds the admission permit"
        );

        task.abort();
        let join = task.await;
        assert!(
            join.is_err(),
            "the dropped future does not complete normally"
        );

        // Cancelling the future drops the store/fiber and returns the permit.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut released = false;
        while !released && Instant::now() < deadline {
            released = admission.available_permits() == capacity && handler.in_flight_count() == 0;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(
            released,
            "cancel freed the store and returned the admission permit"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_admission_caps_in_flight_work() {
        let _lock = async_admission_test_lock().await;
        async_admission_quiescent().await;
        let handler = Arc::new(WasmHttpHandler::new());
        let wasm_bytes = wat::parse_str(CPU_SPIN_WAT).unwrap();
        let admission = wasm_admission();
        let capacity = admission.capacity();

        // capacity + 1 slow requests: without the semaphore the peak would be
        // capacity + 1; with it, at most capacity run at once.
        let mut tasks = Vec::new();
        for _ in 0..=capacity {
            let handler = Arc::clone(&handler);
            let bytes = wasm_bytes.clone();
            let req = sample_request();
            let limits = WasmRuntimeLimits::default();
            tasks.push(tokio::spawn(async move {
                handler
                    .execute_module_with_config_and_limits_async(
                        &bytes,
                        &req,
                        &WasiConfig::deny_all(),
                        &limits,
                        &WasmSecretEnv::empty(),
                        execution_deadline(&limits),
                        None,
                    )
                    .await
            }));
        }
        for task in tasks {
            let res = task.await.unwrap().unwrap();
            assert_eq!(res.status, 200);
        }

        assert!(
            handler.peak_in_flight_count() <= capacity as u64,
            "in-flight work stayed inside the admission limit: peak={}",
            handler.peak_in_flight_count()
        );
        assert_eq!(
            admission.available_permits(),
            capacity,
            "every permit came back"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compile_retains_admission_permit_after_caller_cancel() {
        let _lock = async_admission_test_lock().await;
        async_admission_quiescent().await;
        let handler = WasmHttpHandler::new();
        let wasm_bytes = wat::parse_str(LONG_SPIN_WAT).unwrap();
        let admission = wasm_admission();
        let capacity = admission.capacity();

        // Gate the cold compile: while it is parked, the admission permit is
        // owned by the spawn_blocking closure, not by the caller future.
        let fence = Arc::new(compile_fence::Fence::new());
        let _gate = compile_fence::install(fence.clone());

        let task = tokio::spawn(async move {
            handler
                .execute_module_with_config_and_limits_async(
                    &wasm_bytes,
                    &sample_request(),
                    &WasiConfig::deny_all(),
                    &WasmRuntimeLimits::default(),
                    &WasmSecretEnv::empty(),
                    execution_deadline(&WasmRuntimeLimits::default()),
                    None,
                )
                .await
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while !fence.entered() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(fence.entered(), "cold compile reached the gate");
        assert_eq!(
            admission.available_permits(),
            capacity - 1,
            "the compile holds the admission permit"
        );

        // The caller gives up mid-compile: the JoinHandle drops, but the
        // closure keeps the permit until the real compile ends.
        task.abort();
        let _ = task.await;
        assert_eq!(
            admission.available_permits(),
            capacity - 1,
            "dropping the JoinHandle must not release the permit"
        );

        // The real compile finishes and the closure returns the permit.
        fence.release();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut back = false;
        while !back && Instant::now() < deadline {
            back = admission.available_permits() == capacity;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(
            back,
            "the permit came back when the in-flight compile ended"
        );
    }

    /// (EDG-52) a handler call whose shared deadline already elapsed is
    /// refused even on the fast paths (admission ready, module cache hit):
    /// tokio's `timeout_at` would hand back a ready value even past the
    /// deadline, so the explicit deadline checks fail closed. Deterministic
    /// — no sleeps: the deadline is pinned in the past.
    #[tokio::test(flavor = "current_thread")]
    async fn expired_deadline_refuses_before_guest_work() {
        let _lock = async_admission_test_lock().await;
        let handler = WasmHttpHandler::new();
        let wasm_bytes = wat::parse_str(ECHO_URI_WAT).unwrap();
        let req = sample_request();
        let limits = WasmRuntimeLimits::default();

        // Warm the module cache with a live budget: the first call succeeds.
        let warm = handler
            .execute_module_with_config_and_limits_async(
                &wasm_bytes,
                &req,
                &WasiConfig::deny_all(),
                &limits,
                &WasmSecretEnv::empty(),
                execution_deadline(&limits),
                None,
            )
            .await
            .unwrap();
        assert_eq!(warm.status, 200);

        // The same call with a deadline that already elapsed must be a
        // WASM_TIMEOUT, not a success (cache hit, no guest work).
        let past = Instant::now() - Duration::from_millis(1);
        let err = handler
            .execute_module_with_config_and_limits_async(
                &wasm_bytes,
                &req,
                &WasiConfig::deny_all(),
                &limits,
                &WasmSecretEnv::empty(),
                past,
                None,
            )
            .await
            .expect_err("an already-expired deadline must not succeed");
        assert_eq!(err.code, "WASM_TIMEOUT");
    }

    /// (test-only) spins the current-thread runtime without yielding until
    /// `until`: wakes queued during the window (the deadline timer included)
    /// are only observed when the window ends, so a future result that
    /// becomes ready inside the window is the one `within_budget` sees — a
    /// Ready answer observed PAST the deadline, deterministically (no margin
    /// on the deadline side, no flaky sleep).
    fn hold_current_thread_until(until: Instant) {
        while Instant::now() < until {
            std::hint::spin_loop();
        }
    }

    /// (EDG-52 P2) an error that only becomes ready AFTER the deadline must
    /// be classified WASM_TIMEOUT, not mapped to the ordinary guest error:
    /// the epoch callback's error completes the guest future exactly this
    /// way (a Ready error observed past the deadline — not the pre-check of
    /// an already-expired deadline).
    #[tokio::test(flavor = "current_thread")]
    async fn within_budget_maps_late_ready_error_to_timeout() {
        let deadline = Instant::now() + Duration::from_millis(50);
        tokio::spawn(async move {
            hold_current_thread_until(deadline + Duration::from_millis(10));
        });
        let late_err = async {
            tokio::time::sleep(Duration::from_millis(55)).await;
            Err::<i32, wasmtime::Error>(wasmtime::Error::msg("guest error ready past the deadline"))
        };
        let err = within_budget(deadline, late_err, |e| {
            IsolationError::new("WASM_EXEC", e.to_string())
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, "WASM_TIMEOUT");
    }

    /// Control for the late-error classification: an error ready BEFORE the
    /// deadline keeps the ordinary guest mapping (WASM_EXEC) — the deadline
    /// check must not reclassify errors that finished inside the budget.
    #[tokio::test(flavor = "current_thread")]
    async fn within_budget_keeps_error_ready_before_deadline() {
        let deadline = Instant::now() + Duration::from_millis(50);
        let early_err = async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            Err::<i32, wasmtime::Error>(wasmtime::Error::msg("guest trap inside the budget"))
        };
        let err = within_budget(deadline, early_err, |e| {
            IsolationError::new("WASM_EXEC", e.to_string())
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, "WASM_EXEC");
    }

    /// Control for the late-answer branch: a SUCCESS ready past the deadline
    /// is still refused (the pre-existing post-check), so only the error
    /// branch changed — late answers never become successes.
    #[tokio::test(flavor = "current_thread")]
    async fn within_budget_maps_late_ready_success_to_timeout() {
        let deadline = Instant::now() + Duration::from_millis(50);
        tokio::spawn(async move {
            hold_current_thread_until(deadline + Duration::from_millis(10));
        });
        let late_ok = async {
            tokio::time::sleep(Duration::from_millis(55)).await;
            Ok(7)
        };
        let err = within_budget(deadline, late_ok, |e| {
            IsolationError::new("WASM_EXEC", e.to_string())
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, "WASM_TIMEOUT");
    }

    /// Control for the pre-check: an already-elapsed deadline is refused
    /// before the future is even polled — no guest work, no mapping.
    #[tokio::test(flavor = "current_thread")]
    async fn within_budget_refuses_elapsed_deadline_before_poll() {
        let deadline = Instant::now() - Duration::from_millis(1);
        let polled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&polled);
        let fut = async move {
            flag.store(true, Ordering::SeqCst);
            Ok(7)
        };
        let err = within_budget(deadline, fut, |e| {
            IsolationError::new("WASM_EXEC", e.to_string())
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, "WASM_TIMEOUT");
        assert!(
            !polled.load(Ordering::SeqCst),
            "the pre-check must not poll the guest future"
        );
    }

    /// (EDG-52) a compile that only FAILS after the deadline is a late
    /// result: the absolute deadline classifies it WASM_TIMEOUT, never
    /// WASM_COMPILE (the `?` on the join result must not run ahead of the
    /// deadline check). The compile parks at the gate before the deadline;
    /// a raw thread releases it after the deadline while this runtime is
    /// held, so the (failing) compiler runs inside the window and its error
    /// becomes Ready past the deadline — observed only when the hold ends.
    #[tokio::test(flavor = "current_thread")]
    async fn late_compile_error_is_timeout_not_compile() {
        let _lock = async_admission_test_lock().await;
        async_admission_quiescent().await;
        let handler = Arc::new(WasmHttpHandler::new());
        // Valid magic, broken body: the inline validation passes; the real
        // compiler inside the blocking task fails (WASM_COMPILE).
        let broken: Vec<u8> = b"\0asm\x01\x02\x03".to_vec();
        let fence = Arc::new(compile_fence::Fence::new());
        let _gate = compile_fence::install(fence.clone());
        let deadline = Instant::now() + Duration::from_millis(100);

        let task = tokio::spawn(async move {
            handler
                .execute_module_with_config_and_limits_async(
                    &broken,
                    &sample_request(),
                    &WasiConfig::deny_all(),
                    &WasmRuntimeLimits::default(),
                    &WasmSecretEnv::empty(),
                    deadline,
                    None,
                )
                .await
        });

        // The cold compile parks at the gate BEFORE the deadline: the
        // request is admitted and the join future is armed.
        let started = Instant::now();
        while !fence.entered() && started.elapsed() < Duration::from_millis(500) {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(fence.entered(), "the cold compile reached the gate");

        // Release the gate a little AFTER the deadline from a raw thread (the
        // blocking compile pool is outside this runtime); hold this runtime
        // until past the deadline so the armed timeout is observed only
        // after the join result is ready.
        std::thread::spawn(move || {
            let release_at = deadline + Duration::from_millis(5);
            let wait = release_at.saturating_duration_since(Instant::now());
            std::thread::sleep(wait);
            fence.release();
        });
        tokio::spawn(async move {
            hold_current_thread_until(deadline + Duration::from_millis(10));
        });

        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err.code, "WASM_TIMEOUT");
    }

    /// (EDG-52) the secret value REPLACES a public env entry of the same
    /// name — exactly one entry per name — and private values are present
    /// even with `allow_env = false` (which grants no fs/net/stdio either
    /// way); the public-only path keeps its traditional behavior.
    #[test]
    fn combined_env_secret_wins_and_allow_env_gates_public_only() {
        let mut secrets = WasmSecretEnv::empty();
        secrets.insert("APP_SECRET", "s3cret".to_owned());
        let mut public = HashMap::new();
        public.insert("PUB".to_owned(), "public".to_owned());
        public.insert("APP_SECRET".to_owned(), "public-secret".to_owned());
        let mut wasi = WasiConfig::deny_all();
        wasi.env = public;

        // allow_env = false: only the private value, no public entries.
        let entries = combined_env(&wasi, &secrets);
        assert_eq!(
            entries,
            vec![("APP_SECRET".to_owned(), "s3cret".to_owned())]
        );

        // allow_env = true: the duplicate is resolved by the secret — one
        // entry per name, deterministically sorted.
        wasi.allow_env = true;
        let entries = combined_env(&wasi, &secrets);
        assert_eq!(
            entries,
            vec![
                ("APP_SECRET".to_owned(), "s3cret".to_owned()),
                ("PUB".to_owned(), "public".to_owned()),
            ]
        );

        // No secrets: the pre-existing public-env behavior is unchanged.
        let entries = combined_env(&wasi, &WasmSecretEnv::empty());
        assert_eq!(
            entries,
            vec![
                ("APP_SECRET".to_owned(), "public-secret".to_owned()),
                ("PUB".to_owned(), "public".to_owned()),
            ]
        );
    }

    /// (EDG-52) the deadline covers the WHOLE budget (resolution +
    /// admission + compile + execution) and the arithmetic never panics on
    /// extreme timeouts (checked/saturating, overflow fails closed).
    #[test]
    fn execution_deadline_uses_the_full_budget() {
        let limits = WasmRuntimeLimits::default();
        let before = std::time::Instant::now();
        let deadline = execution_deadline(&limits);
        let elapsed = deadline - before;
        let budget = total_execution_budget(&limits);
        assert!(
            elapsed >= budget && elapsed <= budget + Duration::from_millis(200),
            "deadline must be now + the full budget: {elapsed:?} vs {budget:?}"
        );

        // zero timeout keeps the one-tick watchdog (10 ms); the lower bound
        // is measured against `before` (the deadline's own `now` is never
        // earlier than it).
        let zero = WasmRuntimeLimits {
            epoch_deadline_ticks: epoch_ticks_for_timeout(0),
            ..Default::default()
        };
        let before = std::time::Instant::now();
        let deadline = execution_deadline(&zero);
        let elapsed = deadline - before;
        assert!(
            elapsed >= Duration::from_millis(10)
                && elapsed <= Duration::from_millis(10) + Duration::from_millis(200)
        );

        // extreme timeout: no panic, still a valid (fail-closed) deadline
        let extreme = WasmRuntimeLimits {
            epoch_deadline_ticks: epoch_ticks_for_timeout(u64::MAX),
            ..Default::default()
        };
        let deadline = execution_deadline(&extreme);
        assert!(deadline >= std::time::Instant::now());
    }

    fn sample_request() -> SerializedRequest {
        SerializedRequest {
            method: "GET".into(),
            uri: "/".into(),
            headers: vec![],
            body: None,
            request_id: "wasm-unit".into(),
            base_href: None,
        }
    }

    // (EDG-52 07a) the sync embedding denies the HTTP import — even for a
    // module already cached by a granted async execution on the same
    // handler: the per-execution policy re-check runs on the cache hit with
    // http_granted = false, and a cold sync compile is denied at compile
    // time. WASIp1 legacy imports stay linked as before.
    const HTTP_FETCH_WAT: &str = r#"
        (module
          (import "edger_http_v1" "fetch"
            (func $fetch (param i32 i32 i32 i32) (result i64)))
          (memory (export "memory") 1)
          ;; request frame @ 100: 16-byte prefix, m=3, u=2, h=2, b=0
          (data (i32.const 100) "\03\00\00\00\02\00\00\00\02\00\00\00\00\00\00\00GET/x[]")

          (func (export "edger_alloc") (param $len i32) (result i32)
            (local $ptr i32)
            i32.const 10000
            local.set $ptr
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
            (local $r i64)
            i32.const 100
            i32.const 23
            i32.const 4000
            i32.const 4096
            call $fetch
            local.set $r
            local.get $r
            i64.const 13
            i64.lt_s
            if
              unreachable
            end
            i32.const 40000
            i32.const 200
            i32.store16
            i32.const 40004
            i32.const 2
            i32.store
            i32.const 40008
            i32.const 2
            i32.store
            i32.const 40012
            i32.const 91
            i32.store8
            i32.const 40013
            i32.const 93
            i32.store8
            i32.const 40014
            local.get $r
            i64.const 0xFFFF
            i64.and
            i32.wrap_i64
            i32.store16
            i32.const 40000
            i64.extend_i32_u
            i64.const 16
            i64.const 32
            i64.shl
            i64.or
          )
        )
    "#;

    /// (test-only) always answers a minimal valid response: status 200,
    /// no headers, no body — a 14-byte frame the fixture can pass through.
    struct OkHttpTransport;

    #[async_trait::async_trait]
    impl crate::wasm::http::WasmHttpTransport for OkHttpTransport {
        async fn fetch(
            &self,
            _request: crate::wasm::http::WasmHttpRequest,
            _response_body_limit: usize,
        ) -> Result<crate::wasm::http::WasmHttpResponse, crate::wasm::http::WasmHttpError> {
            Ok(crate::wasm::http::WasmHttpResponse {
                status: 200,
                headers: vec![],
                body: bytes::Bytes::new(),
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sync_embedding_denies_http_import_even_with_cached_async_module() {
        let _lock = async_admission_test_lock().await;
        let handler = WasmHttpHandler::new();
        let wasm_bytes = wat::parse_str(HTTP_FETCH_WAT).unwrap();
        let grant = WasmHttpExecution {
            transport: Arc::new(OkHttpTransport),
            request_id: "wasm-http-inline".into(),
        };
        let req = sample_request();
        let limits = WasmRuntimeLimits::default();

        // Granted async execution: the import is linked and the module is
        // cached; the guest sees the 14-byte success frame (14, not a
        // negative code).
        let res = handler
            .execute_module_with_config_and_limits_async(
                &wasm_bytes,
                &req,
                &WasiConfig::deny_all(),
                &limits,
                &WasmSecretEnv::empty(),
                execution_deadline(&limits),
                Some(&grant),
            )
            .await
            .expect("a granted async execution of the HTTP import must succeed");
        assert_eq!(res.status, 200);
        assert_eq!(
            res.body.as_ref().map(|b| b.as_ref()),
            Some(b"\x0e\x00".as_ref())
        );

        // Sync path on the SAME handler: the cached module is still denied
        // (cache hit re-check with http_granted = false).
        let err = handler
            .execute_module_with_config_and_limits(
                &wasm_bytes,
                &req,
                &WasiConfig::deny_all(),
                &limits,
            )
            .expect_err("the sync embedding must deny the HTTP import on a cache hit");
        assert_eq!(err.code, "WASM_IMPORT_DENIED");

        // A cold sync compile of the same module is denied before
        // instantiation, on a fresh handler with an empty cache.
        let fresh = WasmHttpHandler::new();
        let err = fresh
            .execute_module_with_config_and_limits(
                &wasm_bytes,
                &req,
                &WasiConfig::deny_all(),
                &limits,
            )
            .expect_err("the sync embedding must deny the HTTP import on a cold compile");
        assert_eq!(err.code, "WASM_IMPORT_DENIED");
    }
}
