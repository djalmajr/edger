//! Minimal Wasm HTTP response ABI (story 07.05 v1).
//!
//! Module exports:
//! - `http_status() -> i32`
//! - `http_body_len() -> i32`
//! - `memory` — body bytes at offset 0

use crate::wasm::WasiConfig;
use edger_core::{IsolationError, SerializedResponse};
use wasmtime::{Engine, Instance, Module, Store};

const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_MODULE_BYTES: usize = 4 * 1024 * 1024;

pub struct WasmHttpHandler {
    engine: Engine,
}

impl WasmHttpHandler {
    pub fn new() -> Self {
        Self {
            engine: Engine::default(),
        }
    }

    pub fn execute_module(&self, wasm_bytes: &[u8]) -> Result<SerializedResponse, IsolationError> {
        self.execute_module_with_config(wasm_bytes, &WasiConfig::deny_all())
    }

    pub fn execute_module_with_config(
        &self,
        wasm_bytes: &[u8],
        wasi: &WasiConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        validate_module_bytes(wasm_bytes)?;
        let module = Module::new(&self.engine, wasm_bytes)
            .map_err(|e| IsolationError::new("WASM_COMPILE", e.to_string()))?;
        validate_import_policy(&module, wasi)?;

        let mut store = Store::new(&self.engine, ());
        let instance = Instance::new(&mut store, &module, &[])
            .map_err(|e| IsolationError::new("WASM_INSTANTIATE", e.to_string()))?;

        let status = call_i32_export(&mut store, &instance, "http_status").unwrap_or(200);
        let body_len = call_i32_export(&mut store, &instance, "http_body_len").unwrap_or(0);
        if body_len < 0 {
            return Err(IsolationError::new(
                "WASM_ABI",
                "http_body_len must be non-negative",
            ));
        }
        let body_len = body_len as usize;
        if body_len > MAX_BODY_BYTES {
            return Err(IsolationError::new(
                "WASM_ABI",
                format!("body length {body_len} exceeds cap {MAX_BODY_BYTES}"),
            ));
        }

        let body = if body_len == 0 {
            None
        } else {
            let memory = instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| IsolationError::new("WASM_ABI", "memory export missing"))?;
            let data = memory
                .data(&store)
                .get(..body_len)
                .ok_or_else(|| IsolationError::new("WASM_ABI", "body read out of bounds"))?;
            Some(bytes::Bytes::copy_from_slice(data))
        };

        Ok(SerializedResponse {
            status: u16::try_from(status).unwrap_or(200),
            headers: vec![("content-type".into(), "text/plain".into())],
            body,
        })
    }
}

impl Default for WasmHttpHandler {
    fn default() -> Self {
        Self::new()
    }
}

fn call_i32_export(
    store: &mut Store<()>,
    instance: &Instance,
    name: &str,
) -> Result<i32, IsolationError> {
    let func = instance
        .get_typed_func::<(), i32>(&mut *store, name)
        .map_err(|e| IsolationError::new("WASM_ABI", format!("export {name}: {e}")))?;
    func.call(&mut *store, ())
        .map_err(|e| IsolationError::new("WASM_EXEC", e.to_string()))
}

fn validate_import_policy(module: &Module, wasi: &WasiConfig) -> Result<(), IsolationError> {
    if let Some(import) = module.imports().next() {
        let module_name = import.module();
        let name = import.name();
        if is_wasi_module(module_name) {
            let code = if wasi.is_restricted() {
                "WASI_IMPORT_DENIED"
            } else {
                "WASI_IMPORT_UNSUPPORTED"
            };
            return Err(IsolationError::new(
                code,
                format!("WASI import {module_name}::{name} is not available in ABI v1"),
            ));
        }
        return Err(IsolationError::new(
            "WASM_IMPORT_DENIED",
            format!("host import {module_name}::{name} is not allowed"),
        ));
    }
    Ok(())
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

fn is_wasi_module(module: &str) -> bool {
    module == "wasi_snapshot_preview1" || module.starts_with("wasi:")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (data (i32.const 0) "wasm-hello")
          (func (export "http_status") (result i32) i32.const 200)
          (func (export "http_body_len") (result i32) i32.const 10)
        )
    "#;

    #[test]
    fn executes_minimal_http_abi() {
        let wasm_bytes = wat::parse_str(HELLO_WAT).unwrap();
        let handler = WasmHttpHandler::new();
        let res = handler.execute_module(&wasm_bytes).unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(
            res.body.as_ref().map(|b| b.as_ref()),
            Some(b"wasm-hello".as_ref())
        );
    }

    #[test]
    fn rejects_invalid_wasm_magic() {
        let handler = WasmHttpHandler::new();
        let err = handler.execute_module(b"not-wasm").unwrap_err();
        assert_eq!(err.code, "WASM_INVALID");
    }

    #[test]
    fn rejects_oversized_wasm_module() {
        let mut wasm_bytes = b"\0asm".to_vec();
        wasm_bytes.resize(MAX_MODULE_BYTES + 1, 0);

        let handler = WasmHttpHandler::new();
        let err = handler.execute_module(&wasm_bytes).unwrap_err();

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
        let err = handler.execute_module(&wasm_bytes).unwrap_err();

        assert_eq!(err.code, "WASM_IMPORT_DENIED");
    }

    #[test]
    fn rejects_wasi_imports_when_sandbox_is_deny_all() {
        let wasm_bytes = wat::parse_str(
            r#"
            (module
              (import "wasi_snapshot_preview1" "fd_read" (func $fd_read))
            )
            "#,
        )
        .unwrap();

        let handler = WasmHttpHandler::new();
        let err = handler
            .execute_module_with_config(&wasm_bytes, &WasiConfig::deny_all())
            .unwrap_err();

        assert_eq!(err.code, "WASI_IMPORT_DENIED");
    }
}
