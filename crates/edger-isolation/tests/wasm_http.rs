//! (EDG-52 07a) Behavioral tests of the versioned guest HTTP import
//! `edger_http_v1.fetch`: real WAT calling the import against a fake,
//! observable host transport — no servers, no ports, no Deno/sqld.
//!
//! This test binary has its OWN process-wide admission pool (separate from
//! the lib test binary): the two tests that assert on the GLOBAL admission
//! permit count (`deadline_*`, `dropping_execution_*`) serialize on the
//! binary-local `global_lock` below and wait for quiescence; the other
//! tests never assert global state and may run in parallel (each holds at
//! most one short permit).

#![cfg(feature = "wasm")]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use edger_core::{parse_worker_config, EgressManifest, Isolate, WorkerConfig, WorkerManifest};
use edger_isolation::wasm::{
    WasiConfig, WasmHttpError, WasmHttpRequest, WasmHttpResponse, WasmHttpTransport, WasmIsolate,
};
use tokio::sync::MutexGuard;

// ---------------------------------------------------------------------------
// Guest fixtures
// ---------------------------------------------------------------------------

/// Guest probe: `edger_handle` pre-fills the out region @ 4000 with 0xCC,
/// calls `fetch` with the substituted (req_ptr, req_len, out_ptr, out_cap),
/// and on a NEGATIVE result verifies the out region is byte-for-byte
/// unmodified (no partial write) and reports the code as a 2-byte i16 LE
/// body; on success it copies the response frame from the out region and
/// returns it as the execution's own response frame.
const PROBE_WAT: &str = r#"
    (module
      (import "edger_http_v1" "fetch"
        (func $fetch (param i32 i32 i32 i32) (result i64)))
      (memory (export "memory") {pages})
      {data}

      (func (export "edger_alloc") (param $len i32) (result i32)
        i32.const 20000
      )

      (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
        (local $r i64)
        (local $i i32)

        ;; pre-fill the out region @ 4000 with 0xCC
        block $fill_done
          loop $fill
            local.get $i
            i32.const {out_cap}
            i32.lt_s
            if
              i32.const 4000
              local.get $i
              i32.add
              i32.const 204
              i32.store8
              local.get $i
              i32.const 1
              i32.add
              local.set $i
              br $fill
            end
            br $fill_done
          end
        end

        i32.const {req_ptr}
        i32.const {req_len}
        i32.const {out_ptr}
        i32.const {out_cap}
        call $fetch
        local.set $r

        local.get $r
        i64.const 13
        i64.lt_s
        if (result i64)
          i32.const 0
          local.set $i
          ;; negative: the out region must be UNTOUCHED (still all 0xCC)
          block $check_done
            loop $check
              local.get $i
              i32.const {out_cap}
              i32.lt_s
              if
                i32.const 4000
                local.get $i
                i32.add
                i32.load8_u
                i32.const 204
                i32.ne
                if
                  unreachable
                end
                local.get $i
                i32.const 1
                i32.add
                local.set $i
                br $check
              end
              br $check_done
            end
          end
          ;; report body = i16 LE of the negative code:
          ;; status 200, headers "[]" (2), body 2 bytes @ 40014
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
        else
          i32.const 0
          local.set $i
          ;; success: copy the r-byte response frame from @ 4000
          block $copy_done
            loop $copy
              local.get $i
              (i32.wrap_i64 (local.get $r))
              i32.lt_s
              if
                i32.const 40000
                local.get $i
                i32.add
                i32.const 4000
                local.get $i
                i32.add
                i32.load8_u
                i32.store8
                local.get $i
                i32.const 1
                i32.add
                local.set $i
                br $copy
              end
              br $copy_done
            end
          end
          i32.const 40000
          i64.extend_i32_u
          local.get $r
          i64.const 32
          i64.shl
          i64.or
        end
      )
    )
"#;

/// The valid request frame @ 100 (151 bytes): method POST, a URL with a
/// query, two header pairs (including Authorization) and a body — every
/// field carries a canary the privacy test must never see in a log.
const SUCCESS_DATA: &str = r#"
      (data (i32.const 100)
        "\04\00\00\00\2B\00\00\00\4D\00\00\00\0B\00\00\00POSThttps://upstream.test/search?q=canary-query[[\"Authorization\",\"Bearer canary-token\"],[\"Content-Type\",\"application/json\"]]body-canary")
"#;

/// The 16-byte LE prefix + payload of a guest request frame, built from
/// the REAL byte length of every field (no hand-counted JSON/URI lengths);
/// the fixture never calls a product encoder.
fn frame_bytes(method: &[u8], uri: &[u8], headers: &[u8], body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(16 + method.len() + uri.len() + headers.len() + body.len());
    for len in [method.len(), uri.len(), headers.len(), body.len()] {
        frame.extend_from_slice(&(len as u32).to_le_bytes());
    }
    for field in [method, uri, headers, body] {
        frame.extend_from_slice(field);
    }
    frame
}

/// A `(data ...)` segment holding the frame at `addr`, every byte WAT-
/// escaped as `\XX` so hostile bytes cannot break the string literal.
fn data_segment(addr: i32, frame: &[u8]) -> String {
    let escaped: String = frame.iter().map(|b| format!("\\{:02X}", b)).collect();
    format!("(data (i32.const {addr}) \"{escaped}\")")
}

/// The headers JSON of `n` small `("a", "b")` pairs, encoded by serde_json:
/// the fixture measures the REAL serialized size instead of hand-counting
/// it.
fn pairs_json(n: usize) -> String {
    let pairs: Vec<(String, String)> = (0..n).map(|_| ("a".into(), "b".into())).collect();
    serde_json::to_string(&pairs).expect("small ASCII pairs always serialize")
}

/// A VALID headers JSON of exactly `target` bytes with 100 ASCII pairs:
/// measure serde_json's base encoding of 100 empty values, then spread the
/// remaining padding over the 100 values (each value stays small); the
/// re-serialization is measured, so no escaping is hand-computed.
fn headers_json_of_exact_size(target: usize) -> String {
    const PAIRS: usize = 100;
    let names: Vec<String> = (0..PAIRS).map(|i| format!("h{i:02}")).collect();
    let base: Vec<(String, String)> = names.iter().map(|n| (n.clone(), String::new())).collect();
    let base_len = serde_json::to_string(&base)
        .expect("empty pairs always serialize")
        .len();
    assert!(
        target >= base_len,
        "the target {target} fits below the base {base_len}"
    );
    let pad = target - base_len;
    let (per, extra) = (pad / PAIRS, pad % PAIRS);
    let pairs: Vec<(String, String)> = names
        .into_iter()
        .enumerate()
        .map(|(i, n)| (n, "p".repeat(per + usize::from(i < extra))))
        .collect();
    let json = serde_json::to_string(&pairs).expect("ASCII pairs always serialize");
    assert_eq!(
        json.len(),
        target,
        "the padded JSON must be exactly {target} bytes"
    );
    json
}

fn probe_wat(
    data: &str,
    req_ptr: i32,
    req_len: i32,
    out_ptr: i32,
    out_cap: i32,
    pages: u32,
) -> String {
    PROBE_WAT
        .replace("{data}", data)
        .replace("{req_ptr}", &req_ptr.to_string())
        .replace("{req_len}", &req_len.to_string())
        .replace("{out_ptr}", &out_ptr.to_string())
        .replace("{out_cap}", &out_cap.to_string())
        .replace("{pages}", &pages.to_string())
}

// ---------------------------------------------------------------------------
// Fake transport (observable, deterministic; never touches the network)
// ---------------------------------------------------------------------------

/// The request as received by the transport, copied field by field
/// (`WasmHttpRequest` deliberately has no `Debug`/`Serialize`).
#[derive(Clone)]
struct RecordedRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    request_id: String,
}

enum FakeBehavior {
    /// Answer immediately with the full response.
    Immediate {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// Fail with the given stable error class.
    Error(WasmHttpError),
    /// Await forever (until the deadline cut or a drop cancels the future).
    Stalling,
    /// Sleep `delay` (tokio) then answer.
    Delayed {
        delay_ms: u64,
        status: u16,
        body: Vec<u8>,
    },
}

struct FakeState {
    behavior: FakeBehavior,
    calls: usize,
    limit_seen: usize,
    request: Option<RecordedRequest>,
    started: Arc<AtomicBool>,
    dropped: Arc<AtomicUsize>,
}

/// (test-only) deterministic transport: records the request fields and the
/// per-call body limit, then acts per the configured behavior. The drop
/// counter flips when the in-flight `fetch` future is dropped (deadline cut
/// or execution cancellation) — the "exactly once" signal.
struct FakeTransport {
    state: Arc<Mutex<FakeState>>,
}

struct DropGuard(Arc<AtomicUsize>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl FakeTransport {
    fn new(behavior: FakeBehavior) -> (Arc<Self>, Arc<AtomicBool>, Arc<AtomicUsize>) {
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicUsize::new(0));
        let self_arc = Arc::new(Self {
            state: Arc::new(Mutex::new(FakeState {
                behavior,
                calls: 0,
                limit_seen: 0,
                request: None,
                started: Arc::clone(&started),
                dropped: Arc::clone(&dropped),
            })),
        });
        (self_arc, started, dropped)
    }

    fn calls(&self) -> usize {
        self.state.lock().unwrap().calls
    }

    fn limit_seen(&self) -> usize {
        self.state.lock().unwrap().limit_seen
    }

    fn recorded(&self) -> Option<RecordedRequest> {
        self.state.lock().unwrap().request.clone()
    }

    /// The host-side handle: the upcast `Arc<dyn WasmHttpTransport>` the
    /// `WasmIsolate` builder takes (keeps the concrete handle usable for
    /// the assertions after the execution).
    fn arc(self: &Arc<Self>) -> Arc<dyn WasmHttpTransport> {
        let upcast: Arc<dyn WasmHttpTransport> = self.clone();
        upcast
    }
}

#[async_trait::async_trait]
impl WasmHttpTransport for FakeTransport {
    async fn fetch(
        &self,
        request: WasmHttpRequest,
        response_body_limit: usize,
    ) -> Result<WasmHttpResponse, WasmHttpError> {
        // Record and release the lock BEFORE any await: the lock must not
        // be held across a suspension point.
        let (behavior, started, dropped) = {
            let mut state = self.state.lock().unwrap();
            state.calls += 1;
            state.limit_seen = response_body_limit;
            state.request = Some(RecordedRequest {
                method: request.method.clone(),
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.to_vec(),
                request_id: request.request_id.clone(),
            });
            let behavior = match &state.behavior {
                FakeBehavior::Immediate {
                    status,
                    headers,
                    body,
                } => FakeBehavior::Immediate {
                    status: *status,
                    headers: headers.clone(),
                    body: body.clone(),
                },
                FakeBehavior::Error(err) => FakeBehavior::Error(*err),
                FakeBehavior::Stalling => FakeBehavior::Stalling,
                FakeBehavior::Delayed {
                    delay_ms,
                    status,
                    body,
                } => FakeBehavior::Delayed {
                    delay_ms: *delay_ms,
                    status: *status,
                    body: body.clone(),
                },
            };
            (
                behavior,
                Arc::clone(&state.started),
                Arc::clone(&state.dropped),
            )
        };
        match behavior {
            FakeBehavior::Immediate {
                status,
                headers,
                body,
            } => Ok(WasmHttpResponse {
                status,
                headers,
                body: Bytes::from(body),
            }),
            FakeBehavior::Error(err) => Err(err),
            FakeBehavior::Stalling => {
                started.store(true, Ordering::SeqCst);
                let _drop_guard = DropGuard(dropped);
                // Never resolves: only the deadline cut or a drop ends this.
                std::future::pending::<()>().await;
                unreachable!("pending never resolves")
            }
            FakeBehavior::Delayed {
                delay_ms,
                status,
                body,
            } => {
                let _drop_guard = DropGuard(dropped);
                started.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                Ok(WasmHttpResponse {
                    status,
                    headers: vec![],
                    body: Bytes::from(body),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Config / request helpers
// ---------------------------------------------------------------------------

/// A per-call worker config WITH the explicit egress grant (the manifest
/// `egress:` requires `allowNet` declared; an empty declaration suffices).
fn egress_config(max_body_size: Option<&str>, timeout_ms: u64) -> WorkerConfig {
    let mut manifest = WorkerManifest {
        name: "wasm-http-t".into(),
        kind: Some("wasm".into()),
        allow_net: Some(vec![]),
        egress: Some(EgressManifest {
            enabled: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    manifest.max_body_size = max_body_size.map(str::to_owned);
    let mut config = parse_worker_config(&manifest);
    config.timeout_ms = timeout_ms;
    config
}

/// The same worker WITHOUT the egress grant (no explicit grant = denied).
fn no_egress_config(timeout_ms: u64) -> WorkerConfig {
    let manifest = WorkerManifest {
        name: "wasm-http-t".into(),
        kind: Some("wasm".into()),
        ..Default::default()
    };
    let mut config = parse_worker_config(&manifest);
    config.timeout_ms = timeout_ms;
    config
}

fn sample_request() -> edger_core::SerializedRequest {
    edger_core::SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "req-http-1".into(),
        base_href: None,
    }
}

/// The response body of a probe negative: the 2-byte i16 LE guest code
/// (sign-extended to the stable i64 code).
fn guest_code(res: &edger_core::SerializedResponse) -> i64 {
    let body = res
        .body
        .as_ref()
        .expect("the probe always reports a body")
        .as_ref();
    assert_eq!(body.len(), 2, "the probe reports a 2-byte code: {body:?}");
    i64::from(i16::from_le_bytes([body[0], body[1]]))
}

/// (test-only, this binary's OWN pool) serializes the tests that assert on
/// the process-wide admission permit count of THIS test binary.
async fn global_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let lock = LOCK.get_or_init(|| tokio::sync::Mutex::new(()));
    lock.lock().await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// (EDG-52 07a) Red → Green: a real WAT module calls the import with a
/// granted binding; the transport sees method/URL (query included)/headers
/// (including Authorization)/body preserved and the HOST request_id, and
/// the guest passes the response frame through. The per-call body limit is
/// `min(limits.response_body_bytes, effective_out_cap - 14)`.
#[tokio::test(flavor = "current_thread")]
async fn http_fetch_success_preserves_method_url_query_headers_body_and_request_id() {
    let (transport, _started, _dropped) = FakeTransport::new(FakeBehavior::Immediate {
        status: 201,
        headers: vec![
            ("content-type".into(), "application/json".into()),
            ("x-proof".into(), "from-transport".into()),
        ],
        body: b"upstream-ok".to_vec(),
    });
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport.arc())
        .with_wasm_bytes(wasm_bytes);
    let res = isolate
        .execute_wasm(sample_request(), &egress_config(None, 30_000))
        .await
        .expect("a granted execution must succeed");

    // The guest passed the transport's response frame through untouched.
    assert_eq!(res.status, 201);
    assert_eq!(
        res.headers,
        vec![
            ("content-type".into(), "application/json".into()),
            ("x-proof".into(), "from-transport".into())
        ]
    );
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"upstream-ok".as_ref())
    );

    // The transport saw every guest field preserved + the host request_id.
    let recorded = transport.recorded().expect("the transport was called");
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.url, "https://upstream.test/search?q=canary-query");
    assert_eq!(
        recorded.headers,
        vec![
            (
                "Authorization".to_string(),
                "Bearer canary-token".to_string()
            ),
            ("Content-Type".to_string(), "application/json".to_string())
        ]
    );
    assert_eq!(recorded.body, b"body-canary");
    assert_eq!(recorded.request_id, "req-http-1");
    // out_cap 4096: limit = min(4 MiB default, 4096 - 14) = 4082.
    assert_eq!(transport.limit_seen(), 4082);
}

/// (EDG-52 07a) No explicit egress grant in the per-call config: the known
/// HTTP import fails closed with WASM_IMPORT_DENIED (the transport binding
/// alone grants nothing).
#[tokio::test(flavor = "current_thread")]
async fn http_import_denied_without_egress_grant() {
    let (transport, _started, _dropped) = FakeTransport::new(FakeBehavior::Immediate {
        status: 200,
        headers: vec![],
        body: vec![],
    });
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport)
        .with_wasm_bytes(wasm_bytes);
    let err = isolate
        .execute_wasm(sample_request(), &no_egress_config(30_000))
        .await
        .expect_err("without the explicit grant the import must be denied");
    assert_eq!(err.code, "WASM_IMPORT_DENIED");
}

/// (EDG-52 07a) No transport binding: the grant alone grants nothing —
/// WASM_IMPORT_DENIED.
#[tokio::test(flavor = "current_thread")]
async fn http_import_denied_without_transport_binding() {
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all()).with_wasm_bytes(wasm_bytes);
    let err = isolate
        .execute_wasm(sample_request(), &egress_config(None, 30_000))
        .await
        .expect_err("without the binding the import must be denied");
    assert_eq!(err.code, "WASM_IMPORT_DENIED");
}

/// (EDG-52 07a) Cache hit after an allowed call: the module is cached by
/// the granted call; a later call on the SAME isolate WITHOUT the grant is
/// still denied (the import policy is re-executed per execution with the
/// current grant — no privilege caching).
#[tokio::test(flavor = "current_thread")]
async fn http_import_denied_on_cache_hit_after_allowed_call() {
    let (transport, _started, _dropped) = FakeTransport::new(FakeBehavior::Immediate {
        status: 200,
        headers: vec![],
        body: vec![],
    });
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport.arc())
        .with_wasm_bytes(wasm_bytes);

    // Allowed: the module enters the (per-handler) cache.
    let res = isolate
        .execute_wasm(sample_request(), &egress_config(None, 30_000))
        .await
        .expect("the first, granted call must succeed");
    assert_eq!(res.status, 200);

    // Same isolate, same module bytes, NO grant on this call: denied on
    // the cache hit.
    let err = isolate
        .execute_wasm(sample_request(), &no_egress_config(30_000))
        .await
        .expect_err("a later ungranted call must be denied on the cache hit");
    assert_eq!(err.code, "WASM_IMPORT_DENIED");

    // The transport was called exactly once (only by the allowed call).
    assert_eq!(transport.calls(), 1);
}

/// (EDG-52 07a) A different name or signature in the HTTP module is NOT
/// released — even with grant + binding: WASM_IMPORT_DENIED at compile
/// time, and the transport is never consulted.
#[tokio::test(flavor = "current_thread")]
async fn http_import_wrong_name_or_signature_denied() {
    for (label, wat_) in [
        (
            "wrong name",
            r#"(module
              (import "edger_http_v1" "fetch2"
                (func (param i32 i32 i32 i32) (result i64)))
              (memory (export "memory") 1)
            )"#,
        ),
        (
            "wrong signature",
            r#"(module
              (import "edger_http_v1" "fetch"
                (func (param i32) (result i32)))
              (memory (export "memory") 1)
            )"#,
        ),
    ] {
        let (transport, _started, _dropped) = FakeTransport::new(FakeBehavior::Immediate {
            status: 200,
            headers: vec![],
            body: vec![],
        });
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_http_transport(transport.arc())
            .with_wasm_bytes(wat::parse_str(wat_).unwrap());
        let err = isolate
            .execute_wasm(sample_request(), &egress_config(None, 30_000))
            .await
            .expect_err(&format!("{label} must be denied"));
        assert_eq!(err.code, "WASM_IMPORT_DENIED");
        assert_eq!(transport.calls(), 0);
    }
}

/// (EDG-52 07a) Hostile request frames (the guest as attacker): every
/// class fails closed with the stable `-2` BEFORE the transport is
/// consulted — sign/overflow/range of every argument, frame cap, OOB
/// read, prefix overflow, exact-length mismatch, invalid UTF-8, invalid
/// JSON, header-count and header-JSON caps, and the out range.
///
/// Every case generates its OWN module and data segment (no two cases
/// share guest memory, so one case cannot corrupt another's frame), and
/// every field that is not the targeted defect is valid — so each frame
/// reaches the check the case names (the 101-pair frame is not clobbered
/// by a later segment; the out-range case parses a VALID request first).
/// The three positive boundary controls (frame at the exact 256 KiB cap,
/// 100 header pairs, headers JSON at the exact 64 KiB cap) are fully
/// valid frames that MUST reach the transport (one call, its `-4`).
/// Each case asserts its code AND its transport call count on its OWN
/// transport/instance — not just a final global counter.
#[tokio::test(flavor = "current_thread")]
async fn malformed_request_frames_fail_closed_without_transport_call() {
    struct Case {
        label: &'static str,
        data: String,
        req_ptr: i32,
        req_len: i32,
        out_ptr: i32,
        out_cap: i32,
        pages: u32,
        expect: i64,
        calls: usize,
    }

    let (invalid, transport_code) = (WasmHttpError::INVALID_REQUEST, WasmHttpError::TRANSPORT);

    // Hostile frames: each is valid in every field except the targeted one.
    let bad_method = frame_bytes(b"\xFF\xFE", b"https://upstream.test/x", b"[]", b"");
    let short_prefix = frame_bytes(b"GET", b"https://upstream.test/x", b"[]", b"")[..8].to_vec();
    let mut trailing_bytes = frame_bytes(b"GET", b"/x", b"[]", b"");
    trailing_bytes.extend_from_slice(b"///");
    // metadataOverflow: prefix lengths that overflow the checked u32 sum,
    // kept explicitly hostile (a u32::MAX method length).
    let mut prefix_overflow = Vec::with_capacity(16);
    prefix_overflow.extend_from_slice(&u32::MAX.to_le_bytes());
    prefix_overflow.extend_from_slice(&1u32.to_le_bytes());
    prefix_overflow.extend_from_slice(&0u32.to_le_bytes());
    prefix_overflow.extend_from_slice(&0u32.to_le_bytes());
    let not_json = frame_bytes(b"GET", b"/x", b"not json", b"");
    let pairs_101 = pairs_json(101);
    let pairs_100 = pairs_json(100);
    let headers_65536 = headers_json_of_exact_size(64 * 1024);
    let headers_65537 = headers_json_of_exact_size(64 * 1024 + 1);
    let pairs_frame_101 = frame_bytes(b"GET", b"/x", pairs_101.as_bytes(), b"");
    let pairs_frame_100 = frame_bytes(b"GET", b"/x", pairs_100.as_bytes(), b"");
    let headers_frame_65536 = frame_bytes(b"GET", b"/x", headers_65536.as_bytes(), b"");
    let headers_frame_65537 = frame_bytes(b"GET", b"/x", headers_65537.as_bytes(), b"");
    // Cap frames: `GET /x []` + a body sized to the exact boundary
    // (16-byte prefix + 3 + 2 + 2 + body = frame length).
    const FRAME_FIXED: usize = 16 + 3 + 2 + 2;
    let cap_frame = |body: usize| frame_bytes(b"GET", b"/x", b"[]", &vec![b'A'; body]);
    let frame_cap_plus_one = cap_frame(256 * 1024 + 1 - FRAME_FIXED);
    let frame_at_cap = cap_frame(256 * 1024 - FRAME_FIXED);

    let cases: Vec<Case> = vec![
        Case {
            label: "invalid UTF-8 in the method",
            data: data_segment(100, &bad_method),
            req_ptr: 100,
            req_len: bad_method.len() as i32,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        Case {
            label: "frame shorter than the 16-byte prefix",
            data: data_segment(100, &short_prefix),
            req_ptr: 100,
            req_len: 8,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        Case {
            label: "exact-length mismatch (trailing bytes)",
            data: data_segment(100, &trailing_bytes),
            req_ptr: 100,
            req_len: trailing_bytes.len() as i32,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        // Syntactically valid frame, body filling cap + 1; the memory
        // (8 pages) and the layout (data @ 65536, far from the out region
        // @ 4000, edger_alloc @ 20000 and the response @ 40000) are
        // sufficient, so the CAP check — not an OOB read — rejects.
        Case {
            label: "request frame cap overflow",
            data: data_segment(65_536, &frame_cap_plus_one),
            req_ptr: 65_536,
            req_len: frame_cap_plus_one.len() as i32,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 8,
            expect: invalid,
            calls: 0,
        },
        // The VALID 151-byte request: reading 131_000 bytes from @100 runs
        // past the 2-page memory (131_100 > 131_072) before any parsing.
        Case {
            label: "request read out of bounds",
            data: SUCCESS_DATA.to_string(),
            req_ptr: 100,
            req_len: 131_000,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        Case {
            label: "prefix lengths overflow the checked sum",
            data: data_segment(100, &prefix_overflow),
            req_ptr: 100,
            req_len: 16,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        Case {
            label: "headers field is not JSON",
            data: data_segment(100, &not_json),
            req_ptr: 100,
            req_len: not_json.len() as i32,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        // 101 pairs with VALID JSON and no other segment in the module:
        // the COUNT check — not JSON decoding — rejects.
        Case {
            label: "header count above the wire cap",
            data: data_segment(100, &pairs_frame_101),
            req_ptr: 100,
            req_len: pairs_frame_101.len() as i32,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        // 65537-byte VALID JSON (100 ASCII pairs), its own module: the
        // header-JSON size cap rejects before any decode.
        Case {
            label: "headers JSON above the 64 KiB cap",
            data: data_segment(65_536, &headers_frame_65537),
            req_ptr: 65_536,
            req_len: headers_frame_65537.len() as i32,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 3,
            expect: invalid,
            calls: 0,
        },
        // Argument sign/range: every one with the VALID 151-byte
        // SUCCESS_DATA, so the ARGUMENT check — not request parsing —
        // rejects (the probe's sentinels stay inside the memory).
        Case {
            label: "negative req_ptr",
            data: SUCCESS_DATA.to_string(),
            req_ptr: -1,
            req_len: 151,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        Case {
            label: "negative req_len",
            data: SUCCESS_DATA.to_string(),
            req_ptr: 100,
            req_len: -1,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        Case {
            label: "negative out_ptr",
            data: SUCCESS_DATA.to_string(),
            req_ptr: 100,
            req_len: 151,
            out_ptr: -1,
            out_cap: 4096,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        Case {
            label: "negative out_cap",
            data: SUCCESS_DATA.to_string(),
            req_ptr: 100,
            req_len: 151,
            out_ptr: 4000,
            out_cap: -1,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        Case {
            label: "out_cap below the 14-byte frame floor",
            data: SUCCESS_DATA.to_string(),
            req_ptr: 100,
            req_len: 151,
            out_ptr: 4000,
            out_cap: 13,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        // The VALID request + an out range past the 2-page memory: the
        // request parses, then the OUTPUT preflight (out_ptr + out_cap vs
        // the live memory size) rejects — it must not fail UTF-8 first.
        Case {
            label: "output range out of bounds",
            data: SUCCESS_DATA.to_string(),
            req_ptr: 100,
            req_len: 151,
            out_ptr: 131_000,
            out_cap: 100,
            pages: 2,
            expect: invalid,
            calls: 0,
        },
        // Positive boundary controls: fully valid frames that MUST reach
        // the transport (exactly one call, its stable -4).
        Case {
            label: "control: request frame at the exact 256 KiB cap",
            data: data_segment(65_536, &frame_at_cap),
            req_ptr: 65_536,
            req_len: frame_at_cap.len() as i32,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 8,
            expect: transport_code,
            calls: 1,
        },
        Case {
            label: "control: header count at the 100-pair wire cap",
            data: data_segment(100, &pairs_frame_100),
            req_ptr: 100,
            req_len: pairs_frame_100.len() as i32,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 2,
            expect: transport_code,
            calls: 1,
        },
        Case {
            label: "control: headers JSON at the exact 64 KiB cap",
            data: data_segment(65_536, &headers_frame_65536),
            req_ptr: 65_536,
            req_len: headers_frame_65536.len() as i32,
            out_ptr: 4000,
            out_cap: 4096,
            pages: 3,
            expect: transport_code,
            calls: 1,
        },
    ];

    let config = egress_config(None, 30_000);
    for case in &cases {
        // Each case gets its OWN transport + instance, so the per-case
        // call count is meaningful (0 for the negatives, 1 for the
        // controls).
        let (transport, _started, _dropped) =
            FakeTransport::new(FakeBehavior::Error(WasmHttpError::Transport));
        let wasm_bytes = wat::parse_str(probe_wat(
            &case.data,
            case.req_ptr,
            case.req_len,
            case.out_ptr,
            case.out_cap,
            case.pages,
        ))
        .unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_http_transport(transport.arc())
            .with_wasm_bytes(wasm_bytes);
        let res = isolate
            .execute_wasm(sample_request(), &config)
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "{}: the execution must reach the guest, got {err:?}",
                    case.label
                )
            });
        assert_eq!(
            guest_code(&res),
            case.expect,
            "{}: the guest must see the stable code",
            case.label
        );
        assert_eq!(
            transport.calls(),
            case.calls,
            "{}: the transport must be consulted {} time(s)",
            case.label,
            case.calls
        );
    }
}

/// (EDG-52 07a) Response caps and cut: a body above the per-call limit
/// (driven by the guest `out_cap` or by the worker body cap) answers `-3`
/// with NO partial write (the guest verifies the out region is untouched);
/// a body that fits EXACTLY succeeds; the transport was told the right
/// limit.
#[tokio::test(flavor = "current_thread")]
async fn response_body_cap_cut_returns_negative_without_partial_write() {
    // (i) out_cap 1024 → limit 1010; a 5000-byte body must be refused.
    let (transport, _started, _dropped) = FakeTransport::new(FakeBehavior::Immediate {
        status: 200,
        headers: vec![],
        body: vec![0xAB; 5000],
    });
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 1024, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport.arc())
        .with_wasm_bytes(wasm_bytes);
    let res = isolate
        .execute_wasm(sample_request(), &egress_config(None, 30_000))
        .await
        .expect("the guest must observe the -3, not a host error");
    assert_eq!(guest_code(&res), WasmHttpError::RESPONSE_TOO_LARGE);
    // The transport was told the per-call limit: min(4 MiB, 1024 - 14).
    assert_eq!(transport.limit_seen(), 1010);

    // (ii) worker body cap 256 bytes → limit 256 regardless of out_cap.
    let (transport2, _s, _d) = FakeTransport::new(FakeBehavior::Immediate {
        status: 200,
        headers: vec![],
        body: vec![0xAB; 300],
    });
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport2.arc())
        .with_wasm_bytes(wasm_bytes);
    let res = isolate
        .execute_wasm(sample_request(), &egress_config(Some("256b"), 30_000))
        .await
        .expect("the guest must observe the -3, not a host error");
    assert_eq!(guest_code(&res), WasmHttpError::RESPONSE_TOO_LARGE);
    assert_eq!(transport2.limit_seen(), 256);

    // (iii) a body that fits exactly (1010 bytes in a 1024 out_cap, empty
    // headers → 12 + 2 + 1010 = 1024) succeeds.
    let (transport3, _s, _d) = FakeTransport::new(FakeBehavior::Immediate {
        status: 200,
        headers: vec![],
        body: vec![0xCD; 1010],
    });
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 1024, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport3)
        .with_wasm_bytes(wasm_bytes);
    let res = isolate
        .execute_wasm(sample_request(), &egress_config(None, 30_000))
        .await
        .unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.body.as_ref().map(|b| b.len()), Some(1010));
    assert!(res.body.unwrap().iter().all(|&b| b == 0xCD));
}

/// (EDG-52 07a) An invalid transport response (outside the status range or
/// violating the header contract) is `-4`; a header set that passes the
/// contract but whose JSON exceeds the 64 KiB frame cap is `-3`.
#[tokio::test(flavor = "current_thread")]
async fn invalid_transport_response_is_denied() {
    let cases: Vec<(FakeBehavior, i64, &str)> = vec![
        (
            FakeBehavior::Immediate {
                status: 999,
                headers: vec![],
                body: vec![],
            },
            WasmHttpError::TRANSPORT,
            "status above 599",
        ),
        (
            FakeBehavior::Immediate {
                status: 99,
                headers: vec![],
                body: vec![],
            },
            WasmHttpError::TRANSPORT,
            "status below 100",
        ),
        (
            FakeBehavior::Immediate {
                status: 200,
                // 101 pairs: above the 100-pair header contract.
                headers: vec![("x-h".to_string(), "v".to_string()); 101],
                body: vec![],
            },
            WasmHttpError::TRANSPORT,
            "header count above the wire contract",
        ),
        (
            FakeBehavior::Immediate {
                status: 200,
                // 100 pairs of (10-char name, 644-char value): the RAW
                // total is 65400 ≤ 64 KiB (contract passes) but the JSON
                // is 66102 > 64 KiB (frame cap).
                headers: vec![("x-abcdefgh".to_string(), "a".repeat(644)); 100],
                body: vec![],
            },
            WasmHttpError::RESPONSE_TOO_LARGE,
            "headers JSON above the 64 KiB frame cap",
        ),
    ];
    for (behavior, expected, label) in cases {
        let (transport, _s, _d) = FakeTransport::new(behavior);
        let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_http_transport(transport)
            .with_wasm_bytes(wasm_bytes);
        let res = isolate
            .execute_wasm(sample_request(), &egress_config(None, 30_000))
            .await
            .unwrap_or_else(|err| panic!("{label}: the guest must observe the code, got {err:?}"));
        assert_eq!(guest_code(&res), expected, "{label}");
    }
}

/// (EDG-52 07a) The transport's own stable error classes map 1:1 to the
/// guest codes (and nothing is written to the guest buffer).
#[tokio::test(flavor = "current_thread")]
async fn transport_error_codes_map_stably_to_guest_codes() {
    for (behavior, expected) in [
        (
            FakeBehavior::Error(WasmHttpError::Unavailable),
            WasmHttpError::UNAVAILABLE,
        ),
        (
            FakeBehavior::Error(WasmHttpError::InvalidRequest),
            WasmHttpError::INVALID_REQUEST,
        ),
        (
            FakeBehavior::Error(WasmHttpError::ResponseTooLarge),
            WasmHttpError::RESPONSE_TOO_LARGE,
        ),
        (
            FakeBehavior::Error(WasmHttpError::Transport),
            WasmHttpError::TRANSPORT,
        ),
    ] {
        let (transport, _s, _d) = FakeTransport::new(behavior);
        let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_http_transport(transport)
            .with_wasm_bytes(wasm_bytes);
        let res = isolate
            .execute_wasm(sample_request(), &egress_config(None, 30_000))
            .await
            .unwrap();
        assert_eq!(guest_code(&res), expected);
    }
}

/// (EDG-52 07a) A failing fetch is logged WITHOUT payload: the captured
/// warn carries only the stable code — never the URL, query, header
/// values or body (the canaries of the request frame). The capture runs
/// on the process-GLOBAL subscriber (installed once, by this test only)
/// with a per-thread armed slot: a scoped subscriber was flaky here —
/// the dispatch can bypass it while the guest drives host callbacks — and
/// the global subscriber is the stable dispatch fallback.
#[tokio::test(flavor = "current_thread")]
async fn redacted_error_never_leaks_request_payload_in_logs() {
    // Install the process-wide WARN capture ONCE (only this test does).
    let capture = ensure_global_warn_capture();

    // Warmup: one equivalent -4 call BEFORE the capture is armed, on a
    // separate transport + isolate: it settles the callsite against the
    // global subscriber; the slot is still None, so its warn is NOT
    // captured and cannot be confused with the one below.
    {
        let (warmup, _s, _d) = FakeTransport::new(FakeBehavior::Error(WasmHttpError::Transport));
        let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
        let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
            .with_http_transport(warmup)
            .with_wasm_bytes(wasm_bytes);
        let res = isolate
            .execute_wasm(sample_request(), &egress_config(None, 30_000))
            .await
            .unwrap();
        assert_eq!(guest_code(&res), WasmHttpError::TRANSPORT);
    }

    // Arm the capture on THIS OS thread only (the current-thread runtime
    // runs the host callbacks here); the guard stays alive until after
    // the asserts and clears the slot on drop, even on panic.
    let events: Arc<Mutex<Vec<CapturedEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let _guard = CaptureGuard::new(&capture.slot, Arc::clone(&events));

    // Control: a warn with the SAME field shape (`code=-4`) emitted on a
    // SEPARATE OS thread (static message, joined below) — the ThreadId
    // boundary must ignore it; it never reaches `events`.
    let control = std::thread::spawn(|| {
        tracing::warn!(
            code = -4,
            "wasm-http-07a control warn from a foreign thread"
        );
    });
    control.join().expect("the control thread joined");

    // The real failed fetch on this thread: exactly one captured warn.
    let (transport, _s, _d) = FakeTransport::new(FakeBehavior::Error(WasmHttpError::Transport));
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport)
        .with_wasm_bytes(wasm_bytes);
    let res = isolate
        .execute_wasm(sample_request(), &egress_config(None, 30_000))
        .await
        .unwrap();
    assert_eq!(guest_code(&res), WasmHttpError::TRANSPORT);

    let captured = events.lock().unwrap().clone();
    let warns: Vec<_> = captured
        .iter()
        .filter(|(_, fields)| fields.iter().any(|f| f == "code=-4"))
        .collect();
    assert_eq!(warns.len(), 1, "one warn per failed fetch, code -4");
    let canaries = [
        "upstream.test",
        "canary-query",
        "canary-token",
        "body-canary",
        "POST",
    ];
    for (target, fields) in &warns {
        assert!(target.contains("edger_isolation"), "{target}");
        for field in fields {
            for canary in canaries {
                assert!(
                    !field.contains(canary),
                    "a request payload leaked into the log: {field}"
                );
            }
        }
    }
}

/// (EDG-52 07a) The transport await is a HOST await inside the guest call:
/// a Tokio heartbeat keeps progressing while the transport sleeps, and the
/// call completes with the transport's answer (the await is inside the
/// shared budget, not a blocked thread).
#[tokio::test(flavor = "current_thread")]
async fn transport_await_heartbeat_progresses_and_completes() {
    let (transport, _started, _dropped) = FakeTransport::new(FakeBehavior::Delayed {
        delay_ms: 150,
        status: 200,
        body: b"after-delay".to_vec(),
    });
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport)
        .with_wasm_bytes(wasm_bytes);

    let ticks = Arc::new(AtomicUsize::new(0));
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
    let res = isolate
        .execute_wasm(sample_request(), &egress_config(None, 30_000))
        .await
        .unwrap();
    let wall = started.elapsed();
    heartbeat.abort(); // this task only, by its recorded handle

    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"after-delay".as_ref())
    );
    assert!(
        wall >= Duration::from_millis(150),
        "transport delay: {wall:?}"
    );
    assert!(
        wall < Duration::from_secs(3),
        "well inside the budget: {wall:?}"
    );
    assert!(
        ticks.load(Ordering::SeqCst) >= 5,
        "the heartbeat progressed while the host await ran"
    );
}

/// (EDG-52 07a) The ONE absolute deadline covers the transport await: a
/// stalled transport is cut by the budget (WASM_TIMEOUT), the host future
/// is dropped EXACTLY ONCE, and the admission permit + Store come back.
#[tokio::test(flavor = "current_thread")]
async fn deadline_cuts_stalled_transport_await_and_drops_it_once() {
    let _lock = global_lock().await;
    let (transport, started, dropped) = FakeTransport::new(FakeBehavior::Stalling);
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport)
        .with_wasm_bytes(wasm_bytes);

    let started_at = Instant::now();
    let err = isolate
        .execute_wasm(sample_request(), &egress_config(None, 300))
        .await
        .expect_err("the shared budget must cut the stalled transport await");
    let wall = started_at.elapsed();
    assert_eq!(err.code, "WASM_TIMEOUT");
    assert!(
        wall < Duration::from_secs(2),
        "the 300 ms budget cut the await, not the test: {wall:?}"
    );
    // The transport was consulted, and its future was dropped exactly once
    // (the deadline cut — no re-drive, no double drop).
    assert!(started.load(Ordering::SeqCst));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

/// (EDG-52 07a) Dropping the request (cancelling the execution) while the
/// transport await is in flight drops the host future EXACTLY ONCE and
/// returns the admission permit + Store.
#[tokio::test(flavor = "current_thread")]
async fn dropping_execution_drops_stalled_transport_future_once() {
    let _lock = global_lock().await;
    let (transport, started, dropped) = FakeTransport::new(FakeBehavior::Stalling);
    let wasm_bytes = wat::parse_str(probe_wat(SUCCESS_DATA, 100, 151, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(transport)
        .with_wasm_bytes(wasm_bytes);
    let config = egress_config(None, 300_000);

    let task = tokio::spawn(async move { isolate.execute_wasm(sample_request(), &config).await });
    // Wait until the transport await is actually in flight.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !started.load(Ordering::SeqCst) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        started.load(Ordering::SeqCst),
        "the transport await started"
    );

    task.abort();
    let join = task.await;
    assert!(
        join.is_err(),
        "the dropped future does not complete normally"
    );
    // Exactly one drop of the host future, promptly.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut count = 0;
    while count != 1 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(2)).await;
        count = dropped.load(Ordering::SeqCst);
    }
    assert_eq!(count, 1, "the host future was dropped exactly once");
}

// ---------------------------------------------------------------------------
// WARN-only tracing capture (the request canaries must never reach it)
//
// One GLOBAL subscriber per test binary (installed once, by the redacted
// test only): the armed slot names the single OS thread whose events are
// recorded; every other thread (parallel tests, the control thread) is
// dropped at the subscriber boundary. No canary/field-value filter.
// ---------------------------------------------------------------------------

/// (test-only) one captured WARN event: the target plus its field values as
/// `name=value` strings.
type CapturedEvent = (String, Vec<String>);

/// The armed capture target: the owning test's OS thread + its events
/// buffer.
#[derive(Clone)]
struct CaptureTarget {
    thread: std::thread::ThreadId,
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

/// The process-wide subscriber: records a WARN event only while the
/// CURRENT thread is the armed target's thread.
struct GlobalWarnCapture {
    slot: Arc<Mutex<Option<CaptureTarget>>>,
}

/// Installs the global WARN capture EXACTLY ONCE for this test binary; a
/// second install (another global subscriber already present) panics with
/// a clear message instead of hiding the error.
fn ensure_global_warn_capture() -> &'static Arc<GlobalWarnCapture> {
    static GLOBAL: OnceLock<Arc<GlobalWarnCapture>> = OnceLock::new();
    GLOBAL.get_or_init(|| {
        let capture = Arc::new(GlobalWarnCapture {
            slot: Arc::new(Mutex::new(None)),
        });
        if tracing::subscriber::set_global_default(capture.clone()).is_err() {
            panic!("a global tracing subscriber was already installed in this test binary; the WARN capture cannot be installed twice");
        }
        capture
    })
}

/// Arms the global capture for the CURRENT OS thread; `Drop` clears the
/// slot (also on panic).
struct CaptureGuard {
    slot: Arc<Mutex<Option<CaptureTarget>>>,
}

impl CaptureGuard {
    fn new(
        slot: &Arc<Mutex<Option<CaptureTarget>>>,
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    ) -> Self {
        *slot.lock().unwrap() = Some(CaptureTarget {
            thread: std::thread::current().id(),
            events,
        });
        Self { slot: slot.clone() }
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        *self.slot.lock().unwrap() = None;
    }
}

impl std::fmt::Debug for GlobalWarnCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GlobalWarnCapture")
    }
}

struct FieldRecorder(Vec<String>);

impl tracing::field::Visit for FieldRecorder {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{field}={value:?}"));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push(format!("{field}={value}"));
    }
}

impl tracing::Subscriber for GlobalWarnCapture {
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
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        // Clone the armed target (the events handle) and release the slot
        // lock BEFORE recording: the slot lock is never held while the
        // events lock is taken (no opposite lock order).
        let Some(target) = self.slot.lock().unwrap().clone() else {
            return;
        };
        // Thread boundary: only the test that armed the target is
        // captured; parallel tests (other threads) are filtered out.
        if std::thread::current().id() != target.thread {
            return;
        }
        let mut recorder = FieldRecorder(Vec::new());
        event.record(&mut recorder);
        target
            .events
            .lock()
            .unwrap()
            .push((event.metadata().target().to_owned(), recorder.0));
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows_from: &tracing::span::Id) {}

    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

// ---------------------------------------------------------------------------
// (EDG-52 07d3a) Private service transport — fixtures and behavioral tests
// ---------------------------------------------------------------------------

/// The URL the fake service adapter admits; anything else is an EXTERNAL
/// URL the bridge denies (the 07d2b service/owner policy lives in the host
/// bridge, never in the guest configuration — this unit only stands it in
/// observably: no network, no DNS, no sqld, [env]).
const SERVICE_URL: &str = "https://service.local/db/query";

/// An out-of-scope target the service adapter must deny before anything
/// else.
const EXTERNAL_URL: &str = "https://external.example/steal";

/// A valid guest request frame for the private-service tests: POST to
/// `url`, one header pair and a canary body, placed @ 100 (`data_segment`
/// escapes every byte, hostile URLs included); returns the WAT segment and
/// the REAL frame length (no hand-counted lengths).
fn service_frame(url: &str) -> (String, i32) {
    let frame = frame_bytes(
        b"POST",
        url.as_bytes(),
        b"[[\"x-scope\",\"worker-wasm-http\"]]",
        b"service-canary",
    );
    (data_segment(100, &frame), frame.len() as i32)
}

struct ServiceState {
    calls: usize,
    limit_seen: usize,
    request: Option<RecordedRequest>,
    fail_with: Option<WasmHttpError>,
}

/// (EDG-52 07d3a) The fake PRIVATE service adapter — the observable stand-in
/// for the 07d2b host bridge: a call counter plus URL scoping. Every URL
/// other than the bound `SERVICE_URL` is rejected with `InvalidRequest`
/// (the stable class the real guard maps a bad-target policy refusal to),
/// the service URL answers a deterministic frame; `failing` makes the
/// service URL fail with a fixed stable class. No network, no sqld
/// ([env]): this unit proves the per-execution SELECTION, not the real
/// bridge's restriction — the real origin negatives are mandatory in the
/// orchestrator.
struct FakeServiceTransport {
    state: Mutex<ServiceState>,
}

impl FakeServiceTransport {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ServiceState {
                calls: 0,
                limit_seen: 0,
                request: None,
                fail_with: None,
            }),
        })
    }

    /// The same adapter failing every service-URL call with `err` (the
    /// counter still sees the attempt).
    fn failing(err: WasmHttpError) -> Arc<Self> {
        let arc = Self::new();
        arc.state.lock().unwrap().fail_with = Some(err);
        arc
    }

    fn calls(&self) -> usize {
        self.state.lock().unwrap().calls
    }

    fn limit_seen(&self) -> usize {
        self.state.lock().unwrap().limit_seen
    }

    fn recorded(&self) -> Option<RecordedRequest> {
        self.state.lock().unwrap().request.clone()
    }

    /// The host-side handle: the upcast `Arc<dyn WasmHttpTransport>` the
    /// `WasmIsolate` builder takes (keeps the concrete handle usable for
    /// the assertions after the execution).
    fn arc(self: &Arc<Self>) -> Arc<dyn WasmHttpTransport> {
        let upcast: Arc<dyn WasmHttpTransport> = self.clone();
        upcast
    }
}

#[async_trait::async_trait]
impl WasmHttpTransport for FakeServiceTransport {
    async fn fetch(
        &self,
        request: WasmHttpRequest,
        response_body_limit: usize,
    ) -> Result<WasmHttpResponse, WasmHttpError> {
        // Record and release the lock BEFORE any decision: the counter and
        // the request fields are the observable evidence (the lock is not
        // held across a suspension point — there is none here, but the
        // scope matches the general fake's contract).
        let fail_with = {
            let mut state = self.state.lock().unwrap();
            state.calls += 1;
            state.limit_seen = response_body_limit;
            state.request = Some(RecordedRequest {
                method: request.method.clone(),
                url: request.url.clone(),
                headers: request.headers.clone(),
                body: request.body.to_vec(),
                request_id: request.request_id.clone(),
            });
            state.fail_with
        };
        if let Some(err) = fail_with {
            return Err(err);
        }
        if request.url != SERVICE_URL {
            // Out-of-scope URL: the bridge denies it before any network
            // (the real 07d2b policy; the class mirrors the real guard's
            // bad-target mapping).
            return Err(WasmHttpError::InvalidRequest);
        }
        Ok(WasmHttpResponse {
            status: 202,
            headers: vec![("x-proof".into(), "from-service-bridge".into())],
            body: Bytes::from(b"service-ok".to_vec()),
        })
    }
}

/// (EDG-52 07d3a) ONLY the private binding, `config.egress` `None`: the
/// import is admitted through the service transport alone — the callback is
/// reached without any egress grant, the request keeps the HOST `request_id`
/// and the service frame is passed through; the per-call config stays
/// grant-free (nothing synthesizes `config.egress`).
#[tokio::test(flavor = "current_thread")]
async fn service_binding_private_only_reaches_service_callback_without_grant() {
    let service = FakeServiceTransport::new();
    let (data, req_len) = service_frame(SERVICE_URL);
    let wasm_bytes = wat::parse_str(probe_wat(&data, 100, req_len, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_service_http_transport(service.arc())
        .with_wasm_bytes(wasm_bytes);
    let config = no_egress_config(30_000);
    assert!(
        config.egress.is_none(),
        "the per-call config is (and stays) without grant"
    );

    let res = isolate
        .execute_wasm(sample_request(), &config)
        .await
        .expect("the private binding alone must admit the import");
    assert_eq!(res.status, 202);
    assert_eq!(
        res.headers,
        vec![("x-proof".into(), "from-service-bridge".into())]
    );
    assert_eq!(
        res.body.as_ref().map(|b| b.as_ref()),
        Some(b"service-ok".as_ref())
    );

    // The service callback was the ONLY one reached, with every guest
    // field preserved and the host request id.
    let recorded = service
        .recorded()
        .expect("the service transport was called");
    assert_eq!(service.calls(), 1);
    assert_eq!(recorded.method, "POST");
    assert_eq!(recorded.url, SERVICE_URL);
    assert_eq!(recorded.body, b"service-canary");
    assert_eq!(recorded.request_id, "req-http-1");
    // out_cap 4096: limit = min(4 MiB default, 4096 - 14) = 4082.
    assert_eq!(service.limit_seen(), 4082);
}

/// (EDG-52 07d3a) Both bindings, `config.egress` `Some`: the GENERAL
/// transport is used and the service callback stays silent. Then the SAME
/// isolate / SAME bytes with `config.egress` `None` (a module cache hit
/// after the general-granted call): the private transport is selected —
/// with ZERO new general callbacks: the grant is never memorized in the
/// isolate/Store/cache, and the cache hit re-selects per execution from
/// the current config.
#[tokio::test(flavor = "current_thread")]
async fn general_with_grant_then_private_without_grant_on_same_isolate_cache() {
    let (general, _started, _dropped) = FakeTransport::new(FakeBehavior::Immediate {
        status: 200,
        headers: vec![],
        body: b"general-ok".to_vec(),
    });
    let service = FakeServiceTransport::new();
    let (data, req_len) = service_frame(SERVICE_URL);
    let wasm_bytes = wat::parse_str(probe_wat(&data, 100, req_len, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(general.arc())
        .with_service_http_transport(service.arc())
        .with_wasm_bytes(wasm_bytes);

    // Grant + both bindings: the general transport serves the call (the
    // orchestrator may compose the general+service router there).
    let res = isolate
        .execute_wasm(sample_request(), &egress_config(None, 30_000))
        .await
        .expect("the general, granted call must succeed");
    assert_eq!(res.status, 200);
    assert_eq!(
        general.calls(),
        1,
        "the general transport served the granted call"
    );
    assert_eq!(
        service.calls(),
        0,
        "the service callback stays silent with a grant"
    );

    // SAME isolate, SAME bytes, NO grant (cache hit after the
    // general-granted call): the private binding serves — and the general
    // transport gets ZERO new callbacks.
    let res = isolate
        .execute_wasm(sample_request(), &no_egress_config(30_000))
        .await
        .expect("the ungranted call must be admitted by the private binding");
    assert_eq!(res.status, 202);
    assert_eq!(service.calls(), 1);
    assert_eq!(
        general.calls(),
        1,
        "zero new general callbacks on the ungranted cache hit"
    );
    assert_eq!(
        service
            .recorded()
            .expect("the service transport was called")
            .url,
        SERVICE_URL
    );
}

/// (EDG-52 07d3a) The private transport fails with the stable `Transport`
/// class: the guest sees the mapped negative code and there is NO
/// automatic fallback to the general transport (the general binding exists
/// but is never consulted after the fetch error).
#[tokio::test(flavor = "current_thread")]
async fn service_transport_error_is_not_a_fallback_to_general() {
    let (general, _started, _dropped) = FakeTransport::new(FakeBehavior::Immediate {
        status: 200,
        headers: vec![],
        body: b"general-ok".to_vec(),
    });
    let service = FakeServiceTransport::failing(WasmHttpError::Transport);
    let (data, req_len) = service_frame(SERVICE_URL);
    let wasm_bytes = wat::parse_str(probe_wat(&data, 100, req_len, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_http_transport(general.arc())
        .with_service_http_transport(service.arc())
        .with_wasm_bytes(wasm_bytes);

    let res = isolate
        .execute_wasm(sample_request(), &no_egress_config(30_000))
        .await
        .expect("a failing fetch is a stable guest code, not an execution error");
    assert_eq!(
        guest_code(&res),
        WasmHttpError::TRANSPORT,
        "the guest sees the stable -4 code"
    );
    assert_eq!(
        service.calls(),
        1,
        "the private transport was consulted once"
    );
    assert_eq!(
        general.calls(),
        0,
        "no automatic fallback to the general transport"
    );
}

/// (EDG-52 07d3a) `config.egress` `Some` but NO general transport bound:
/// the private binding still serves (the selection falls to the service
/// transport whenever the general one is absent) — no denial, no
/// synthesized grant error.
#[tokio::test(flavor = "current_thread")]
async fn service_binding_serves_with_grant_when_general_unbound() {
    let service = FakeServiceTransport::new();
    let (data, req_len) = service_frame(SERVICE_URL);
    let wasm_bytes = wat::parse_str(probe_wat(&data, 100, req_len, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_service_http_transport(service.arc())
        .with_wasm_bytes(wasm_bytes);

    let res = isolate
        .execute_wasm(sample_request(), &egress_config(None, 30_000))
        .await
        .expect("the private binding must serve when the general is unbound");
    assert_eq!(res.status, 202);
    assert_eq!(service.calls(), 1);
    assert_eq!(
        service
            .recorded()
            .expect("the service transport was called")
            .request_id,
        "req-http-1"
    );
}

/// (EDG-52 07d3a) The fake service adapter REJECTS an external URL (the
/// 07d2b bridge's own policy: the selection admits the import, the bridge
/// denies the out-of-scope target) — the counter sees the attempt, the
/// guest gets the stable `-2` code and no response is served. Explicit
/// [env]: this unit does NOT prove the real bridge/network/sqld
/// restriction; the real origin negatives are mandatory in the
/// orchestrator.
#[tokio::test(flavor = "current_thread")]
async fn service_transport_rejects_external_url_with_stable_code() {
    let service = FakeServiceTransport::new();
    let (data, req_len) = service_frame(EXTERNAL_URL);
    let wasm_bytes = wat::parse_str(probe_wat(&data, 100, req_len, 4000, 4096, 1)).unwrap();
    let mut isolate = WasmIsolate::new(WasiConfig::deny_all())
        .with_service_http_transport(service.arc())
        .with_wasm_bytes(wasm_bytes);

    let res = isolate
        .execute_wasm(sample_request(), &no_egress_config(30_000))
        .await
        .expect("the rejection is a stable guest code, not an execution error");
    assert_eq!(
        guest_code(&res),
        WasmHttpError::INVALID_REQUEST,
        "external URL → the stable -2 code"
    );
    let recorded = service.recorded().expect("the counter saw the attempt");
    assert_eq!(service.calls(), 1);
    assert_eq!(recorded.url, EXTERNAL_URL);
}
