//! Linear-memory Wasm HTTP ABI.
//!
//! Module exports:
//! - `memory`
//! - `edger_alloc(len: i32) -> i32`
//! - `edger_handle(ptr: i32, len: i32) -> i64`

use crate::wasm::WasiConfig;
use edger_core::{validate_headers, IsolationError, SerializedRequest, SerializedResponse};
use wasmtime::{Engine, Linker, Module, Store};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

const MAX_FRAME_BYTES: usize = 256 * 1024;
const MAX_MODULE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024;
const REQUEST_HEADER_BYTES: usize = 16;
const RESPONSE_HEADER_BYTES: usize = 12;

pub struct WasmHttpHandler {
    engine: Engine,
}

impl WasmHttpHandler {
    pub fn new() -> Self {
        Self {
            engine: Engine::default(),
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
        validate_module_bytes(wasm_bytes)?;
        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|e| IsolationError::new("WASM_COMPILE", e.to_string()))?;
        validate_import_policy(&module)?;

        let mut linker = Linker::<WasiP1Ctx>::new(&self.engine);
        preview1::add_to_linker_sync(&mut linker, |ctx| ctx)
            .map_err(|e| IsolationError::new("WASI_LINK", e.to_string()))?;
        let mut store = Store::new(&self.engine, build_wasi_context(wasi));
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
        if response_len > MAX_FRAME_BYTES {
            return Err(IsolationError::new(
                "WASM_ABI",
                format!("response frame length {response_len} exceeds cap {MAX_FRAME_BYTES}"),
            ));
        }
        let mut response_frame = vec![0u8; response_len];
        memory
            .read(&store, response_ptr, &mut response_frame)
            .map_err(|e| IsolationError::new("WASM_ABI", format!("response read failed: {e}")))?;

        decode_response_frame(&response_frame)
    }
}

impl Default for WasmHttpHandler {
    fn default() -> Self {
        Self::new()
    }
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
    if frame_len > MAX_FRAME_BYTES {
        return Err(IsolationError::new(
            "WASM_ABI_ENCODE",
            format!("request frame length {frame_len} exceeds cap {MAX_FRAME_BYTES}"),
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

fn decode_response_frame(frame: &[u8]) -> Result<SerializedResponse, IsolationError> {
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
    if body_len > MAX_RESPONSE_BODY_BYTES {
        return Err(IsolationError::new(
            "WASM_ABI_DECODE",
            format!("body length {body_len} exceeds cap {MAX_RESPONSE_BODY_BYTES}"),
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
