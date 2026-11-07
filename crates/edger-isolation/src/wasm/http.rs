//! (EDG-52 07a) Versioned guest HTTP import: `edger_http_v1.fetch`.
//!
//! The guest calls `fetch(req_ptr, req_len, out_ptr, out_cap) -> i64` with
//! the SAME linear-memory frames as the `edger_handle` ABI: the request
//! frame is the existing 16-byte LE prefix (method/URL/headers JSON pairs/
//! body lengths) plus the fields, and the response frame is the existing
//! 12-byte LE prefix (status u16, pad u16, header bytes u32, body bytes
//! u32) plus the JSON header pairs and the body. There is no second JSON
//! format and no Authorization omission: every field the frame carries is
//! preserved end to end.
//!
//! The return value is `>= 14` (the 12-byte header plus the 2-byte empty
//! JSON `[]`) when the full response frame was written into the guest
//! buffer, or one of the stable negative codes of `WasmHttpError` — never a
//! raw serde/URL/header/body error and never the URL, query, headers or
//! body themselves.
//!
//! The transport is a host binding (`WasmHttpTransport`); this module never
//! opens sockets. The host fills `WasmHttpRequest.request_id` from the
//! execution's `SerializedRequest` — the guest frame carries no identity.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use bytes::Bytes;
use edger_core::validate_headers;
use wasmtime::{Caller, Error, Extern, ExternType};

use super::handler::{
    MAX_REQUEST_FRAME_BYTES, MAX_RESPONSE_HEADER_BYTES, REQUEST_HEADER_BYTES, RESPONSE_HEADER_BYTES,
};

/// (EDG-52 07a) The guest import module and function name (exact ABI).
pub(crate) const HTTP_IMPORT_MODULE: &str = "edger_http_v1";
pub(crate) const HTTP_IMPORT_FETCH: &str = "fetch";

/// Smallest possible response frame: the 12-byte header plus the 2-byte
/// empty JSON header list `[]`. `out_cap` below this cannot hold any
/// response and is refused before the transport is called.
pub(crate) const MIN_RESPONSE_FRAME_BYTES: usize = RESPONSE_HEADER_BYTES + 2;

/// (EDG-52 07a) Stable error class of the HTTP transport and import. The
/// variant carries no payload on purpose: a raw error could embed the URL,
/// query, a header value or a body fragment, and none of that may reach a
/// guest, a log line or any `Debug`/`Display` formatting.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum WasmHttpError {
    /// The transport service is unavailable; guest code `-1`.
    Unavailable,
    /// The request (or the guest's arguments) is invalid; guest code `-2`.
    InvalidRequest,
    /// The response exceeds a size cap; guest code `-3`.
    ResponseTooLarge,
    /// A transport-level failure (network, cut, invalid response);
    /// guest code `-4`.
    Transport,
}

impl WasmHttpError {
    /// Guest-visible ABI codes (the negative `fetch` return values).
    pub const UNAVAILABLE: i64 = -1;
    pub const INVALID_REQUEST: i64 = -2;
    pub const RESPONSE_TOO_LARGE: i64 = -3;
    pub const TRANSPORT: i64 = -4;

    /// The stable negative code of this error class (no payload).
    pub fn code(&self) -> i64 {
        match self {
            Self::Unavailable => Self::UNAVAILABLE,
            Self::InvalidRequest => Self::INVALID_REQUEST,
            Self::ResponseTooLarge => Self::RESPONSE_TOO_LARGE,
            Self::Transport => Self::TRANSPORT,
        }
    }
}

/// Constant, payload-free `Display`: the class name only.
impl std::fmt::Display for WasmHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "wasm http transport unavailable",
            Self::InvalidRequest => "wasm http request invalid",
            Self::ResponseTooLarge => "wasm http response too large",
            Self::Transport => "wasm http transport failure",
        })
    }
}

/// Constant, payload-free `Debug` (deliberately not derived): formatting an
/// error must never emit a URL, header value or body.
impl std::fmt::Debug for WasmHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "WasmHttpError::Unavailable",
            Self::InvalidRequest => "WasmHttpError::InvalidRequest",
            Self::ResponseTooLarge => "WasmHttpError::ResponseTooLarge",
            Self::Transport => "WasmHttpError::Transport",
        })
    }
}

/// (EDG-52 07a) One guest HTTP request as seen by the host transport.
///
/// `request_id` is filled by the host from the execution's
/// `SerializedRequest`; it is not part of the guest frame. No `Serialize`
/// and no `Debug` derive on purpose: `method`/`url` (query included), the
/// header pairs and `body` are payload, and they must never be formatted,
/// logged or serialized.
pub struct WasmHttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
    pub request_id: String,
}

/// (EDG-52 07a) The full response the host transport returns for one guest
/// request. Same redaction rule as `WasmHttpRequest`: no derived
/// `Serialize`/`Debug` — the headers and the body are payload.
pub struct WasmHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// (EDG-52 07a) The host-side HTTP transport consumed by the guest import
/// `edger_http_v1.fetch`. Object-safe and async; the real implementation
/// (the egress guard adapter, 07b) runs policy and network OUTSIDE the
/// guest. The isolation layer never dials: a `WasiConfig` with sockets
/// enabled is not what serves these calls.
#[async_trait]
pub trait WasmHttpTransport: Send + Sync {
    /// Perform one guest request and return the FULL response.
    ///
    /// `response_body_limit` is the per-call body cap computed by the
    /// host: `min(limits.response_body_bytes, effective_out_cap - 14)`.
    /// Answering a body above the limit as success is a contract violation
    /// — the host will refuse the frame with `-3` either way; the transport
    /// must cut or fail instead of silently truncating.
    #[allow(clippy::double_must_use)] // the `async_trait` expansion adds a `#[must_use]` on a future return
    async fn fetch(
        &self,
        request: WasmHttpRequest,
        response_body_limit: usize,
    ) -> Result<WasmHttpResponse, WasmHttpError>;
}

/// (EDG-52 07a) The host data of ONE admitted async execution that may use
/// the HTTP import: the transport binding (the host's `Arc`, cloned per
/// execution — the module cache never holds requests, env or futures) and
/// the host-provided request id (the guest frame carries no identity).
/// Deliberately without `Debug`: the transport may hold policy material.
pub(crate) struct WasmHttpExecution {
    pub(crate) transport: Arc<dyn WasmHttpTransport>,
    pub(crate) request_id: String,
}

/// The exact guest signature of `edger_http_v1.fetch`:
/// `(i32, i32, i32, i32) -> i64`. Anything else in the module is denied.
pub(crate) fn is_http_fetch_signature(ty: &ExternType) -> bool {
    let ExternType::Func(func) = ty else {
        return false;
    };
    let params: Vec<_> = func.params().collect();
    let results: Vec<_> = func.results().collect();
    params.len() == 4
        && params.iter().all(|ty| ty.is_i32())
        && results.len() == 1
        && results.iter().all(|ty| ty.is_i64())
}

/// (EDG-52 07a) Host entry point of the guest `fetch` import (async host
/// function). Every argument is validated and the request frame is copied
/// to host memory BEFORE the transport await — no guest-memory borrow
/// crosses the await — and the output range is validated against the live
/// memory size before the network. The transport await runs under the
/// ONE absolute deadline: a late or never-ready answer is the budget's own
/// exhaustion (a trap that the execution maps to `WASM_TIMEOUT`), never a
/// success. On any negative outcome nothing is written to the guest
/// buffer (no partial frame), the failure is logged without payload, and
/// the stable code is returned.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_http<T: Send + 'static>(
    mut caller: Caller<'_, T>,
    transport: Arc<dyn WasmHttpTransport>,
    request_id: String,
    response_body_bytes: usize,
    response_frame_cap: usize,
    deadline: Instant,
    req_ptr: i32,
    req_len: i32,
    out_ptr: i32,
    out_cap: i32,
) -> Result<i64, Error> {
    // -- Arguments: sign/overflow/range, `out_cap >= 14`, all BEFORE the
    // transport is ever consulted.
    let (req_ptr, out_ptr, req_len, out_cap) = match (
        usize::try_from(req_ptr),
        usize::try_from(out_ptr),
        usize::try_from(req_len),
        usize::try_from(out_cap),
    ) {
        (Ok(req_ptr), Ok(out_ptr), Ok(req_len), Ok(out_cap)) => {
            (req_ptr, out_ptr, req_len, out_cap)
        }
        _ => return fail(WasmHttpError::InvalidRequest),
    };
    if out_cap < MIN_RESPONSE_FRAME_BYTES
        || !(REQUEST_HEADER_BYTES..=MAX_REQUEST_FRAME_BYTES).contains(&req_len)
    {
        return fail(WasmHttpError::InvalidRequest);
    }

    let memory = match caller
        .get_export("memory")
        .and_then(|extern_| match extern_ {
            Extern::Memory(memory) => Some(memory),
            _ => None,
        }) {
        Some(memory) => memory,
        None => return fail(WasmHttpError::InvalidRequest),
    };

    // -- Copy the request to host memory (capped before the allocation):
    // from here the request lives in a host buffer, so no guest borrow
    // crosses the transport await below.
    let mut request_frame = vec![0u8; req_len];
    if memory
        .read(&mut caller, req_ptr, &mut request_frame)
        .is_err()
    {
        return fail(WasmHttpError::InvalidRequest);
    }
    let request = match parse_http_request_frame(&request_frame, &request_id) {
        Ok(request) => request,
        Err(err) => return fail(err),
    };

    // -- Output range BEFORE the network: the effective cap is bounded by
    // the execution frame cap; the range is checked against the LIVE byte
    // size of the memory (`data_size` — NOT `size`, which is in wasm pages)
    // overflow-free.
    let effective_cap = out_cap.min(response_frame_cap);
    if effective_cap < MIN_RESPONSE_FRAME_BYTES {
        return fail(WasmHttpError::InvalidRequest);
    }
    let size = memory.data_size(&caller);
    let Some(out_end) = out_ptr.checked_add(effective_cap) else {
        return fail(WasmHttpError::InvalidRequest);
    };
    if out_end > size {
        return fail(WasmHttpError::InvalidRequest);
    }
    let response_body_limit = response_body_bytes.min(effective_cap - MIN_RESPONSE_FRAME_BYTES);

    // -- The deadline BEFORE the transport is even constructed: the
    // preflight above (frame copy/parse, output range) runs under the
    // epoch clock, so the budget can expire in between. Without this check
    // the `timeout_at` below would poll the transport first (tokio polls
    // the wrapped future before its timer) and an outbound effect could
    // start after the budget. The SAME exhaustion trap as the epoch
    // callback and the post-await checks below: the absolute deadline
    // decides, never message sniffing.
    if Instant::now() >= deadline {
        return Err(budget_exhausted());
    }

    // -- The transport, under the ONE absolute deadline (the await is
    // inside the 05b budget). A ready answer — success or error — that
    // only lands after the deadline is refused: the epoch callback traps
    // the guest at this return, and the execution maps it to
    // WASM_TIMEOUT.
    let response = match tokio::time::timeout_at(
        deadline.into(),
        transport.fetch(request, response_body_limit),
    )
    .await
    {
        Ok(Ok(response)) => {
            if Instant::now() >= deadline {
                return Err(budget_exhausted());
            }
            response
        }
        Ok(Err(err)) => {
            if Instant::now() >= deadline {
                return Err(budget_exhausted());
            }
            return fail(err);
        }
        Err(_) => return Err(budget_exhausted()),
    };

    // -- Encode the full response frame; any cap breach (headers/body/
    // whole frame vs the effective out_cap) is `-3` with NOTHING written.
    let frame = match encode_http_response_frame(&response, response_body_limit, effective_cap) {
        Ok(frame) => frame,
        Err(err) => return fail(err),
    };

    // -- Write back with a FRESH guest borrow. The range was validated
    // before the network and guest memory only grows, so this cannot
    // regress; a failure here is a transport-class error, not a success.
    match memory.write(&mut caller, out_ptr, &frame) {
        Ok(()) => Ok(frame.len() as i64),
        Err(_) => fail(WasmHttpError::Transport),
    }
}

/// One negative outcome: a payload-free warn (code only — never the URL,
/// query, headers or body) and the stable guest code.
fn fail(err: WasmHttpError) -> Result<i64, Error> {
    let code = err.code();
    tracing::warn!(code = %code, "wasm http fetch failed");
    Ok(code)
}

fn budget_exhausted() -> Error {
    // The SAME message the epoch callback raises: the absolute budget
    // decides, never message sniffing.
    Error::msg("wasm total execution budget exhausted")
}

/// Parses the guest request frame (the existing 16-byte LE prefix format:
/// method/URL/headers JSON pairs/body lengths + fields) into the host
/// request. Every failure is the generic `InvalidRequest` class: serde
/// and UTF-8 errors can embed input values and are never passed through.
fn parse_http_request_frame(
    frame: &[u8],
    request_id: &str,
) -> Result<WasmHttpRequest, WasmHttpError> {
    const PREFIX: usize = REQUEST_HEADER_BYTES;
    let mut lengths = [0u32; 4];
    for (slot, offset) in lengths.iter_mut().enumerate() {
        let bytes: [u8; 4] = frame[4 * slot..4 * slot + 4]
            .try_into()
            .expect("prefix bounds");
        *offset = u32::from_le_bytes(bytes);
    }
    // Checked: hostile lengths overflow before they reach an allocation.
    let end = PREFIX
        .checked_add(lengths[0] as usize)
        .and_then(|len| len.checked_add(lengths[1] as usize))
        .and_then(|len| len.checked_add(lengths[2] as usize))
        .and_then(|len| len.checked_add(lengths[3] as usize));
    if end != Some(frame.len()) {
        return Err(WasmHttpError::InvalidRequest);
    }
    let (method_len, url_len, headers_len, body_len) = (
        lengths[0] as usize,
        lengths[1] as usize,
        lengths[2] as usize,
        lengths[3] as usize,
    );
    // The headers JSON cap is enforced BEFORE any JSON decode.
    if headers_len > MAX_RESPONSE_HEADER_BYTES {
        return Err(WasmHttpError::InvalidRequest);
    }
    let base = PREFIX;
    let method = str::from_utf8(&frame[base..base + method_len])
        .map_err(|_| WasmHttpError::InvalidRequest)?;
    let url = str::from_utf8(&frame[base + method_len..base + method_len + url_len])
        .map_err(|_| WasmHttpError::InvalidRequest)?;
    let headers_start = base + method_len + url_len;
    let headers: Vec<(String, String)> =
        serde_json::from_slice(&frame[headers_start..headers_start + headers_len])
            .map_err(|_| WasmHttpError::InvalidRequest)?;
    // The existing header validation (count/size caps) of the wire contract.
    validate_headers(&headers).map_err(|_| WasmHttpError::InvalidRequest)?;
    let body_start = headers_start + headers_len;
    let body = Bytes::copy_from_slice(&frame[body_start..body_start + body_len]);
    Ok(WasmHttpRequest {
        method: method.to_owned(),
        url: url.to_owned(),
        headers,
        body,
        request_id: request_id.to_owned(),
    })
}

/// Encodes the transport response into the existing 12-byte LE response
/// frame (status u16, pad u16, header bytes u32, body bytes u32 + JSON
/// header pairs + body). Size-cap breaches are `ResponseTooLarge`
/// (nothing is written); a malformed response (status range, header
/// contract, JSON) is the `Transport` class — never a raw error.
fn encode_http_response_frame(
    response: &WasmHttpResponse,
    response_body_limit: usize,
    effective_cap: usize,
) -> Result<Vec<u8>, WasmHttpError> {
    if !(100..=599).contains(&response.status) {
        return Err(WasmHttpError::Transport);
    }
    if response.body.len() > response_body_limit {
        return Err(WasmHttpError::ResponseTooLarge);
    }
    // The same header contract the response decode enforces: a response
    // the guest could never decode is invalid, not a success.
    validate_headers(&response.headers).map_err(|_| WasmHttpError::Transport)?;
    let headers_json =
        serde_json::to_vec(&response.headers).map_err(|_| WasmHttpError::Transport)?;
    if headers_json.len() > MAX_RESPONSE_HEADER_BYTES {
        return Err(WasmHttpError::ResponseTooLarge);
    }
    let frame_len = RESPONSE_HEADER_BYTES
        .checked_add(headers_json.len())
        .and_then(|len| len.checked_add(response.body.len()))
        .ok_or(WasmHttpError::ResponseTooLarge)?;
    if frame_len > effective_cap {
        return Err(WasmHttpError::ResponseTooLarge);
    }
    let mut frame = Vec::with_capacity(frame_len);
    frame.extend_from_slice(&response.status.to_le_bytes());
    frame.extend_from_slice(&0u16.to_le_bytes());
    frame.extend_from_slice(&(headers_json.len() as u32).to_le_bytes());
    frame.extend_from_slice(&(response.body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&headers_json);
    frame.extend_from_slice(&response.body);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    // (Codex r7 finding 2) The absolute deadline must be checked
    // IMMEDIATELY before the transport is polled: the frame copy/parse and
    // the output preflight run under the epoch clock, so the deadline can
    // expire in between — and `timeout_at` polls the wrapped future before
    // its timer, so the transport could start an outbound effect after the
    // budget. These unit tests drive the REAL `fetch_http` as a guest
    // import of a minimal module (no product pool/handler, no handler
    // precheck, which would mask exactly this path).

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use wasmtime::{Config, Engine, Linker, Module, Store, TypedFunc};

    /// The guest module: the `edger_http_v1.fetch` import, a 1-page memory
    /// holding the minimal valid request frame at 0 (16-byte LE prefix —
    /// method 3, url 2, headers 2, body 0 — plus `GET`/`/x`/`[]`, 23 bytes
    /// total), and `run` calling the import with req(0, 23) and
    /// out(64, 128). No WASI, no `edger_alloc`, no sentinel.
    const GUEST_WAT: &str = r#"
        (module
            (import "edger_http_v1" "fetch"
                (func $fetch (param i32 i32 i32 i32) (result i64)))
            (memory (export "memory") 1)
            (data (i32.const 0)
                "\03\00\00\00\02\00\00\00\02\00\00\00\00\00\00\00GET/x[]")
            (func (export "run") (result i64)
                i32.const 0
                i32.const 23
                i32.const 64
                i32.const 128
                call $fetch)
        )
    "#;

    /// Counting transport: it increments the counter ONLY when the fetch
    /// future is actually polled and answers the stable `Transport` class
    /// — no network, no sleep, no background task.
    struct CountingTransport {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl WasmHttpTransport for CountingTransport {
        async fn fetch(
            &self,
            _request: WasmHttpRequest,
            _response_body_limit: usize,
        ) -> Result<WasmHttpResponse, WasmHttpError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(WasmHttpError::Transport)
        }
    }

    /// One fresh engine, a simple linker that wraps the REAL `fetch_http`
    /// (capturing the deadline) and a `Store<()>`: the local engine/store
    /// never touch the handler's global admission semaphore. (wasmtime 49:
    /// async support is unconditional — the old `async_support(true)`
    /// Config flag is a deprecated no-op here — and a module with an async
    /// host import instantiates only through `instantiate_async`.)
    async fn drive_fetch(
        transport: Arc<CountingTransport>,
        deadline: Instant,
    ) -> (Store<()>, TypedFunc<(), i64>) {
        let engine = Engine::new(&Config::new()).expect("test engine");
        let mut linker = Linker::<()>::new(&engine);
        linker
            .func_wrap_async::<_, (i32, i32, i32, i32), Result<i64, Error>>(
                HTTP_IMPORT_MODULE,
                HTTP_IMPORT_FETCH,
                move |caller, (req_ptr, req_len, out_ptr, out_cap)| {
                    let transport = Arc::clone(&transport);
                    let fut = fetch_http(
                        caller,
                        transport,
                        "deadline-p2-test".to_owned(),
                        64, // response_body_bytes
                        78, // response_frame_cap
                        deadline,
                        req_ptr,
                        req_len,
                        out_ptr,
                        out_cap,
                    );
                    // `func_wrap_async` wants a boxed (not pinned) host
                    // future, exactly like the production linker.
                    Box::new(fut)
                },
            )
            .expect("link the fetch import");
        let module = Module::new(&engine, GUEST_WAT).expect("compile the guest module");
        let mut store = Store::new(&engine, ());
        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .expect("instantiate the guest module");
        let run = instance
            .get_typed_func::<(), i64>(&mut store, "run")
            .expect("typed run export");
        (store, run)
    }

    #[tokio::test]
    async fn expired_deadline_traps_before_the_transport_is_polled() {
        let calls = Arc::new(AtomicUsize::new(0));
        let transport = Arc::new(CountingTransport {
            calls: calls.clone(),
        });
        let deadline = Instant::now() - Duration::from_millis(1);
        let (mut store, run) = drive_fetch(transport, deadline).await;
        let err = run
            .call_async(&mut store, ())
            .await
            .expect_err("an expired deadline must trap the guest, not answer");
        // No message/backtrace sniffing (the project's budget rule in
        // `within_budget`): the absolute deadline decides — an error ready
        // after it is the budget's own exhaustion (WASM_TIMEOUT).
        assert!(
            Instant::now() >= deadline,
            "the deadline must have passed before the error was ready: {err}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the transport must not be polled once the deadline passed"
        );
    }

    #[tokio::test]
    async fn future_deadline_still_polls_the_transport_exactly_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let transport = Arc::new(CountingTransport {
            calls: calls.clone(),
        });
        let (mut store, run) =
            drive_fetch(transport, Instant::now() + Duration::from_secs(30)).await;
        let code = run
            .call_async(&mut store, ())
            .await
            .expect("a future deadline must not trap");
        assert_eq!(
            code,
            WasmHttpError::TRANSPORT,
            "the stable transport code must reach the guest"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the valid frame/fixture must reach the transport exactly once"
        );
    }
}
