//! Linear-memory Wasm HTTP ABI.
//!
//! Module exports:
//! - `memory`
//! - `edger_alloc(len: i32) -> i32`
//! - `edger_handle(ptr: i32, len: i32) -> i64`

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::wasm::WasiConfig;
use edger_core::{
    effective_max_body_size_bytes_usize, validate_headers, IsolationError, SerializedRequest,
    SerializedResponse, WorkerConfig,
};
use sha2::{Digest, Sha256};
use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

const MAX_REQUEST_FRAME_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_HEADER_BYTES: usize = 64 * 1024;
const DEFAULT_RESPONSE_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODULE_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_HEADER_BYTES: usize = 16;
const RESPONSE_HEADER_BYTES: usize = 12;
const DEFAULT_WASM_MEMORY_BYTES: usize = 512 * 1024 * 1024;
const LOW_MEMORY_WASM_MEMORY_BYTES: usize = 128 * 1024 * 1024;
const DEFAULT_WASM_TABLE_ELEMENTS: usize = 10_000;
const DEFAULT_WASM_TIMEOUT_MS: u64 = 30_000;
const WASM_EPOCH_TICK_MS: u64 = 10;

pub struct WasmHttpHandler {
    engine: Engine,
    module_cache: Mutex<HashMap<ModuleCacheKey, Module>>,
}

impl WasmHttpHandler {
    pub fn new() -> Self {
        let engine = build_engine();
        spawn_epoch_ticker(&engine);
        Self {
            engine,
            module_cache: Mutex::new(HashMap::new()),
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

        let mut linker = Linker::<WasmStoreState>::new(&self.engine);
        preview1::add_to_linker_sync(&mut linker, |state| &mut state.wasi)
            .map_err(|e| IsolationError::new("WASI_LINK", e.to_string()))?;
        let mut store = Store::new(&self.engine, WasmStoreState::new(wasi, limits));
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(limits.epoch_deadline_ticks);
        store.epoch_deadline_trap();
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| IsolationError::new("WASM_INSTANTIATE", e.to_string()))?;

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

    fn compiled_module(&self, wasm_bytes: &[u8]) -> Result<Module, IsolationError> {
        validate_module_bytes(wasm_bytes)?;
        let key = ModuleCacheKey::from_bytes(wasm_bytes);
        if let Some(module) = self.module_cache()?.get(&key).cloned() {
            return Ok(module);
        }

        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|e| IsolationError::new("WASM_COMPILE", e.to_string()))?;
        validate_import_policy(&module)?;
        self.module_cache()?.insert(key, module.clone());
        Ok(module)
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

struct WasmStoreState {
    limits: StoreLimits,
    wasi: WasiP1Ctx,
}

impl WasmStoreState {
    fn new(wasi: &WasiConfig, limits: &WasmRuntimeLimits) -> Self {
        Self {
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.memory_bytes)
                .memories(1)
                .table_elements(limits.table_elements)
                .tables(1)
                .build(),
            wasi: build_wasi_context(wasi),
        }
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

fn validate_import_policy(module: &Module) -> Result<(), IsolationError> {
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
        return Err(IsolationError::new(
            "WASM_IMPORT_DENIED",
            format!("host import {module_name}::{name} is not allowed"),
        ));
    }
    Ok(())
}

fn build_wasi_context(wasi: &WasiConfig) -> WasiP1Ctx {
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
    if wasi.allow_env {
        let mut env = wasi.env.iter().collect::<Vec<_>>();
        env.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (key, value) in env {
            builder.env(key, value);
        }
    }
    builder.build_p1()
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
