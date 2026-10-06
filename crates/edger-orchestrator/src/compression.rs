//! HTTP compression of data-plane (app) responses — EDG-2/EDG-3.
//!
//! The `CompressionLayer` (tower-http, brotli + gzip at the default quality
//! level — brotli 4) is mounted in `build_pipeline` outside
//! `owned_host_middleware` and inside `request_metrics_middleware`, so it
//! wraps the router fallback and the owned-host dispatch alike.
//!
//! Policy: only responses produced by `pipeline_handler` — marked with the
//! `AppResponse` extension, including its error responses — are ever
//! compressed. The control plane (health/ready probes, metrics, MCP, admin
//! API, `/` redirect) passes through untouched.
//!
//! The tower-http `DefaultPredicate` is kept (it already excludes
//! `text/event-stream`, gRPC, `image/*` except `image/svg+xml`, and bodies
//! under 32 B) and combined with:
//!
//! - only marked app responses (`AppResponse` extension);
//! - skip when `Cache-Control` carries `no-transform` (case-insensitive,
//!   among the other directives);
//! - skip already-compressed media types: `image/*` (except
//!   `image/svg+xml`), `video/*`, `audio/*`, `font/woff`, `font/woff2`,
//!   `application/zip`, `application/gzip`, `application/x-gzip`,
//!   `application/x-brotli`, `application/zstd`, `application/pdf`,
//!   `application/octet-stream`;
//! - skip when `Content-Disposition` starts with `attachment`;
//! - skip statuses 1xx, 204 and 304;
//! - skip HEAD responses (the inner byte-counting middleware marks them, so
//!   they are never compressed and their metadata — in particular the
//!   `content-length` describing the GET, or its absence on a 304 — passes
//!   through untouched);
//! - minimum compressible size (default 1024 bytes, configurable through
//!   `CompressionConfig::min_bytes`) when the size is known. Unknown-size
//!   (streaming) bodies are compressed — the encoder flushes per chunk.
//!
//! Responses that already carry `content-encoding` or `content-range` are
//! skipped by tower-http itself, before the predicate runs.
//!
//! ## Configuration (EDG-6)
//!
//! `CompressionConfig` (read from the `EDGER_COMPRESSION*` envs in the
//! binary and mounted by `build_pipeline`) controls: whether the layer is
//! mounted at all (`enabled`; `off` means no layer, no 406, no Vary added by
//! the layer), the minimum size and the `CompressionLevel` (`default`,
//! `fastest`, `best` or a precise integer; `best` is brotli quality 11 —
//! the most expensive setting for dynamic/streaming bodies).
//!
//! ## 406 is scoped to apps (EDG-6)
//!
//! tower-http 0.7 answers 406 Not Acceptable (RFC 9110 §12.5.3) for EVERY
//! response — health, ready, metrics, admin — when the `Accept-Encoding`
//! accepts neither a supported coding nor identity (`Encoding::from_headers`
//! returns `None`; the inner response is passed through with only the status
//! overwritten and `Vary: Accept-Encoding` appended when missing). The
//! `compression_request_scope` middleware, mounted outside the layer,
//! detects exactly that unsatisfiable negotiation (mirroring the tower-http
//! decision for the layer's supported set `identity`/`gzip`/`br`), removes
//! the header so the layer negotiates identity instead, and re-applies the
//! 406 ONLY to app responses (`AppResponse` marker) — same body/headers as
//! the tower-http 406. The control plane never answers 406.
//!
//! ## Byte counters (EDG-6)
//!
//! `count_compression_input` (inside the layer) counts the pre-compression
//! body bytes of responses the negotiation could compress. It wraps the body
//! in a `CountingBody` that is a `http_body::Body` delegating `poll_frame`
//! and PRESERVING the original body's `size_hint`/`is_end_stream` — so a
//! known-size body (static asset, pipeline error) still reports its exact
//! size to the layer's `SizeAbove` predicate instead of looking like an
//! unknown-size stream. Every response that reaches the layer WITHOUT a
//! `content-encoding` is marked `ResponseWithoutEncoding`: after the layer,
//! that marker plus a final `br`/`gzip` `content-encoding` proves the layer
//! itself compressed the response — covering BOTH the Ok and the Err
//! branches of `pipeline_handler`. The scope middleware, after the layer,
//! wraps the final body of exactly those responses in a `RecordingStream`
//! and records both totals to `CompressionMetrics`
//! (`edger_http_compression_bytes_in_total{encoding}` / `..._out_total`)
//! when the compressed body ends — including streaming (the encoder flushes
//! per chunk) and abandoned bodies (drop records whatever passed).

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::{Request, State};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Version};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use futures_util::{ready, Stream};
// `Body as _` is REQUIRED for the `poll_frame`/`size_hint`/`is_end_stream`
// method calls on `axum::body::Body` below (verified by compile: removing it
// yields E0599); the unused-imports lint is a false positive for anonymously
// imported traits used only via method resolution.
#[allow(unused_imports)]
use http_body::{Body as _, Frame, SizeHint};
use tower_http::compression::predicate::{DefaultPredicate, Predicate, SizeAbove};
use tower_http::compression::{CompressionLayer, CompressionLevel};

use crate::metrics::{CompressionEncoding, CompressionMetrics};
use crate::pipeline::OrchestratorState;

/// Minimum body size (bytes) for compression when the size is known.
pub const MIN_COMPRESSIBLE_BYTES: u64 = 1024;

/// Data-plane compression settings (EDG-6). Read from the
/// `EDGER_COMPRESSION*` envs in the binary and mounted by `build_pipeline`
/// through the shared server state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressionConfig {
    /// Whether the compression layer (and its 406/byte accounting) is
    /// mounted. `false` means no layer at all: no `content-encoding`, no
    /// 406, no `Vary` added by the layer.
    pub enabled: bool,
    /// Minimum compressible body size in bytes when the size is known.
    pub min_bytes: u64,
    /// Quality level. `best` is brotli quality 11 — the most expensive
    /// setting for dynamic/streaming bodies.
    pub level: CompressionLevel,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_bytes: MIN_COMPRESSIBLE_BYTES,
            level: CompressionLevel::default(),
        }
    }
}

impl CompressionConfig {
    /// Parse the `EDGER_COMPRESSION` value: `on` (default) or `off`,
    /// case-insensitive. Anything else is invalid (the caller warns and
    /// keeps the default).
    pub fn parse_enabled(value: &str) -> Option<bool> {
        match value.trim().to_ascii_lowercase().as_str() {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        }
    }

    /// Parse `EDGER_COMPRESSION_LEVEL`: `default`, `fastest` or `best`
    /// (case-insensitive), or a non-negative integer mapped to
    /// `CompressionLevel::Precise`. Anything else is invalid (the caller
    /// warns and keeps the default).
    pub fn parse_level(value: &str) -> Option<CompressionLevel> {
        let value = value.trim();
        match value.to_ascii_lowercase().as_str() {
            "default" => Some(CompressionLevel::Default),
            "fastest" => Some(CompressionLevel::Fastest),
            "best" => Some(CompressionLevel::Best),
            _ => {
                let non_negative_integer =
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
                non_negative_integer
                    .then(|| value.parse::<i32>().ok())
                    .flatten()
                    .map(CompressionLevel::Precise)
            }
        }
    }
}

/// Marker extension inserted by `pipeline_handler` on every data-plane
/// response it produces (worker responses and pipeline errors alike).
#[derive(Debug, Clone, Copy, Default)]
pub struct AppResponse;

/// Mark `res` as a data-plane (app) response, eligible for compression.
pub fn mark_app_response<B>(res: &mut Response<B>) {
    res.extensions_mut().insert(AppResponse);
}

/// Marker extension inserted by `pipeline_handler` on app responses whose
/// body came straight from the worker WITHOUT a `content-encoding` header.
/// The ETag-weakening middleware uses it to know that compression changed
/// the representation, so a strong worker ETag must be downgraded to weak.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkerResponseWithoutEncoding;

/// Mark `res` as a worker response that carried no `content-encoding`.
pub fn mark_worker_without_encoding<B>(res: &mut Response<B>) {
    res.extensions_mut().insert(WorkerResponseWithoutEncoding);
}

/// Marker extension inserted by the INNER middleware
/// (`count_compression_input`) on every response that reaches the compression
/// layer WITHOUT a `content-encoding` header. After the layer, this marker
/// plus a final `content-encoding` of `br`/`gzip` proves the LAYER added the
/// encoding — i.e. the layer compressed the response. Unlike
/// `WorkerResponseWithoutEncoding` (inserted by `pipeline_handler` on the Ok
/// branch only), it covers BOTH the Ok and the Err branches of the pipeline:
/// pipeline errors are app responses too, and their bodies are compressed
/// alike. Responses whose `content-encoding` came from the worker keep no
/// marker and stay out of the byte counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResponseWithoutEncoding;

/// Mark `res` as having reached the compression layer without a
/// `content-encoding` header.
pub fn mark_response_without_encoding<B>(res: &mut Response<B>) {
    res.extensions_mut().insert(ResponseWithoutEncoding);
}

/// Marker extension marking a HEAD response so the compression layer's
/// predicate skips it. HEAD is never compressed: its metadata passes through
/// untouched — the `content-length` the worker/pipeline sent (the size of the
/// GET) is preserved, and a 304 (which carries none) gains none.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HeadResponse;

/// Mark `res` as a HEAD response (never compressed).
pub(crate) fn mark_head_response<B>(res: &mut Response<B>) {
    res.extensions_mut().insert(HeadResponse);
}

/// Weaken a single opaque entity-tag, operating on raw bytes (no `to_str()`),
/// as nginx does when a representation is transformed.
///
/// The `ETag` field carries ONE entity-tag (RFC 9110 §8.8.3): a comma is a
/// valid character inside the quotes, so the value is never split. Returns
/// `Some(W/ + all original bytes)` when the value is a valid STRONG
/// entity-tag; `None` when the value must stay untouched:
///
/// * already weak — starts with the ASCII prefix `W/` (kept verbatim);
/// * invalid — empty, without quotes, an unclosed quote, or a lowercase
///   `w/` (not the normative weak prefix). Invalid tags are neither
///   weakened nor removed.
///
/// Bytes >= 0x80 (obs-text) are preserved verbatim.
fn weaken_etag(value: &[u8]) -> Option<Vec<u8>> {
    if value.is_empty() || value.starts_with(b"W/") {
        return None;
    }
    let quoted = value.len() >= 2 && value[0] == b'"' && value[value.len() - 1] == b'"';
    if !quoted {
        return None;
    }
    let mut weakened = Vec::with_capacity(value.len() + 2);
    weakened.extend_from_slice(b"W/");
    weakened.extend_from_slice(value);
    Some(weakened)
}

/// Middleware, mounted immediately OUTSIDE `compression_layer()` (between it
/// and `request_metrics_middleware`), that weakens a worker's strong `ETag`
/// once compression changed the content-coding of the response. It fires
/// only when all three hold:
///
/// * the response is an app response (`AppResponse` marker);
/// * the worker produced it WITHOUT a `content-encoding`
///   (`WorkerResponseWithoutEncoding` marker);
/// * the response now carries a `content-encoding` (added by compression).
///
/// The tower-http compression layer rebuilds the response from `parts`, so
/// both markers (response extensions) survive it and are visible here.
///
/// ETag fields are handled byte-wise (`as_bytes` / `from_bytes`, never
/// `to_str`): each repeated `ETag` field is weakened individually by the same
/// rule, and obs-text bytes (>= 0x80) inside a valid strong tag are
/// preserved.
pub async fn weaken_worker_etag(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let transformed = res.extensions().get::<AppResponse>().is_some()
        && res
            .extensions()
            .get::<WorkerResponseWithoutEncoding>()
            .is_some()
        && res.headers().contains_key(header::CONTENT_ENCODING);
    if transformed {
        let etags: Vec<HeaderValue> = res
            .headers()
            .get_all(header::ETAG)
            .iter()
            .cloned()
            .collect();
        let replaced: Vec<HeaderValue> = etags
            .iter()
            .map(|value| {
                weaken_etag(value.as_bytes())
                    .and_then(|bytes| HeaderValue::from_bytes(&bytes).ok())
                    .unwrap_or_else(|| value.clone())
            })
            .collect();
        if replaced != etags {
            let headers = res.headers_mut();
            headers.remove(header::ETAG);
            for value in replaced {
                headers.append(header::ETAG, value);
            }
        }
    }
    res
}

/// The compression layer for `build_pipeline`: brotli + gzip at the default
/// quality and default floor, gated by the EDG-2 policy
/// (`app_response_predicate`). Kept for layer-level tests; the pipeline uses
/// [`compression_layer_with_config`].
pub fn compression_layer() -> CompressionLayer<impl Predicate> {
    compression_layer_with_config(&CompressionConfig::default())
}

/// The compression layer for `build_pipeline` at the configured quality and
/// minimum size (EDG-6), gated by the EDG-2 policy.
pub fn compression_layer_with_config(
    config: &CompressionConfig,
) -> CompressionLayer<impl Predicate> {
    CompressionLayer::new()
        .quality(config.level)
        .compress_when(predicate_for(config.min_bytes))
}

/// EDG-2 policy (decision 4): the tower-http `DefaultPredicate` combined with
/// the data-plane rules listed in the module docs.
pub fn app_response_predicate() -> impl Predicate {
    predicate_for(MIN_COMPRESSIBLE_BYTES)
}

fn predicate_for(min_bytes: u64) -> impl Predicate {
    DefaultPredicate::new().and(SizeAbove::new(min_bytes)).and(
        |status: StatusCode, _version: Version, headers: &HeaderMap, extensions: &Extensions| {
            is_marked_app_response(extensions)
                && !is_head_response(extensions)
                && !has_no_transform(headers)
                && !is_precompressed_content_type(headers)
                && !is_attachment_disposition(headers)
                && !is_bodyless_status(status)
        },
    )
}

fn is_marked_app_response(extensions: &Extensions) -> bool {
    extensions.get::<AppResponse>().is_some()
}

/// HEAD responses are never compressed. The layer's predicate cannot see the
/// request method, so the marker is inserted by `count_compression_input`
/// (inside the layer) on every HEAD response.
fn is_head_response(extensions: &Extensions) -> bool {
    extensions.get::<HeadResponse>().is_some()
}

/// True when any `Cache-Control` header carries the `no-transform`
/// directive, case-insensitively, among the other directives.
fn has_no_transform(headers: &HeaderMap) -> bool {
    headers.get_all(header::CACHE_CONTROL).iter().any(|value| {
        value
            .to_str()
            .ok()
            .is_some_and(|value| cache_control_has_directive(value, "no-transform"))
    })
}

fn cache_control_has_directive(value: &str, directive: &str) -> bool {
    value
        .split(',')
        .map(|part| {
            let token = part.split(';').next().unwrap_or_default();
            token.split('=').next().unwrap_or_default().trim()
        })
        .any(|token| token.eq_ignore_ascii_case(directive))
}

fn is_precompressed_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let media_type = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    (media_type.starts_with("image/") && media_type != "image/svg+xml")
        || media_type.starts_with("video/")
        || media_type.starts_with("audio/")
        || matches!(
            media_type.as_str(),
            "font/woff"
                | "font/woff2"
                | "application/zip"
                | "application/gzip"
                | "application/x-gzip"
                | "application/x-brotli"
                | "application/zstd"
                | "application/pdf"
                | "application/octet-stream"
        )
}

fn is_attachment_disposition(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            let value = value.trim();
            value.eq_ignore_ascii_case("attachment")
                || value.to_ascii_lowercase().starts_with("attachment;")
        })
}

fn is_bodyless_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 100..=199 | 204 | 304)
}

// ---------------------------------------------------------------------------
// Accept-Encoding negotiation (EDG-6: 406 scoped to apps + byte accounting)
// ---------------------------------------------------------------------------

/// The encodings the compression layer can produce, in the same relative
/// order as the tower-http `Encoding` enum (`Identity` < `Gzip` < `Brotli`),
/// which `preferred_encoding` relies on when quality ties. The layer is
/// built with the `compression-br` + `compression-gzip` features only, so
/// `deflate`/`zstd` are NOT supported and are ignored here exactly as the
/// tower-http `Encoding::parse` ignores unsupported codings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Accepted {
    Identity,
    Gzip,
    Br,
}

/// Q-value as tower-http represents it: an integer between 0 and 1000.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Q(u16);

impl Q {
    const ONE: Q = Q(1000);

    /// Parse a q-value as specified by RFC 9110 §5.3.1 — a mirror of the
    /// tower-http 0.7 `QValue::parse` (content_encoding.rs): `q=` followed
    /// by `0`/`1`, optional `.`, at most 3 fractional digits, total <= 1.0.
    fn parse(s: &str) -> Option<Q> {
        let mut chars = s.chars();
        match chars.next() {
            Some('q' | 'Q') => (),
            _ => return None,
        }
        match chars.next() {
            Some('=') => (),
            _ => return None,
        }
        let mut value = match chars.next() {
            Some('0') => 0,
            Some('1') => 1000,
            _ => return None,
        };
        match chars.next() {
            Some('.') => (),
            None => return Some(Q(value)),
            _ => return None,
        }
        let mut factor = 100;
        loop {
            match chars.next() {
                Some(digit @ '0'..='9') => {
                    if factor < 1 {
                        return None;
                    }
                    value += factor * (digit as u16 - '0' as u16);
                }
                None => return (value <= 1000).then_some(Q(value)),
                _ => return None,
            }
            factor /= 10;
        }
    }
}

/// Parse one content-coding token against the layer's supported set
/// (`gzip`/`x-gzip`, `br`, `identity`). Unknown or unsupported codings
/// (`deflate`, `zstd`, anything else) are ignored — the same rule the
/// tower-http `Encoding::parse` applies with the layer's feature set.
fn parse_coding(token: &str) -> Option<Accepted> {
    if token.eq_ignore_ascii_case("gzip") || token.eq_ignore_ascii_case("x-gzip") {
        Some(Accepted::Gzip)
    } else if token.eq_ignore_ascii_case("br") {
        Some(Accepted::Br)
    } else if token.eq_ignore_ascii_case("identity") {
        Some(Accepted::Identity)
    } else {
        None
    }
}

/// The explicit (coding, q) entries of every `Accept-Encoding` field — a
/// mirror of the tower-http 0.7 `encodings` iterator: repeated fields are
/// all considered, entries split on `,`, coding trimmed, an invalid q-value
/// drops the WHOLE entry, missing q means 1.0.
fn accept_entries(headers: &HeaderMap) -> Vec<(Accepted, Q)> {
    let mut entries = Vec::new();
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(text) = value.to_str() else { continue };
        for raw in text.split(',') {
            let mut parts = raw.splitn(2, ';');
            let Some(coding) = parse_coding(parts.next().unwrap().trim()) else {
                continue; // unknown/unsupported coding: ignored
            };
            let q = match parts.next() {
                Some(qpart) => match Q::parse(qpart.trim()) {
                    Some(q) => q,
                    None => continue, // invalid q: the entry is dropped
                },
                None => Q::ONE,
            };
            entries.push((coding, q));
        }
    }
    entries
}

/// The q-value of the FIRST `*` wildcard whose q parses (the tower-http 0.7
/// `wildcard_qvalue` rule: a `*` with an invalid q-value is skipped, not
/// fatal).
fn wildcard_q(headers: &HeaderMap) -> Option<Q> {
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(text) = value.to_str() else { continue };
        for raw in text.split(',') {
            let mut parts = raw.splitn(2, ';');
            let coding = parts.next().unwrap().trim();
            if coding != "*" {
                continue;
            }
            let q = match parts.next() {
                Some(qpart) => match Q::parse(qpart.trim()) {
                    Some(q) => q,
                    None => continue,
                },
                None => Q::ONE,
            };
            return Some(q);
        }
    }
    None
}

/// The best acceptable (coding, q) pair: max by (q, coding) over the entries
/// with q > 0 — a mirror of the tower-http 0.7 `preferred_encoding`.
fn preferred(entries: impl Iterator<Item = (Accepted, Q)>) -> Option<Accepted> {
    entries
        .filter(|(_, q)| q.0 > 0)
        .max_by_key(|&(coding, q)| (q, coding))
        .map(|(coding, _)| coding)
}

/// Negotiate the encoding the tower-http 0.7 compression layer would pick
/// for this request's `Accept-Encoding` against the layer's supported set
/// (`identity`, `gzip`, `br`): a mirror of `preferred_encoding_with_wildcard`
/// (content_encoding.rs) over `Encoding::from_headers`. Returns `None` in
/// exactly the cases where tower-http answers 406 Not Acceptable (RFC 9110
/// §12.5.3).
fn negotiated_encoding(headers: &HeaderMap) -> Option<Accepted> {
    let explicit = accept_entries(headers);
    let Some(wildcard) = wildcard_q(headers) else {
        // No wildcard: only the explicit entries count. Per RFC 9110 §12.5.3
        // an explicitly rejected identity with nothing acceptable is 406;
        // an unspecified identity still allows the plain body.
        let identity_rejected = explicit
            .iter()
            .any(|(coding, q)| *coding == Accepted::Identity && q.0 == 0);
        return match preferred(explicit.into_iter()) {
            Some(coding) => Some(coding),
            None => {
                if identity_rejected {
                    None
                } else {
                    Some(Accepted::Identity)
                }
            }
        };
    };
    // With a wildcard: each supported coding takes its explicit q when
    // listed (FIRST explicit entry wins, as tower-http's `find` does),
    // otherwise the wildcard q.
    let effective = [Accepted::Identity, Accepted::Gzip, Accepted::Br]
        .into_iter()
        .map(|coding| {
            let q = explicit
                .iter()
                .find(|(explicit_coding, _)| *explicit_coding == coding)
                .map(|(_, q)| *q)
                .unwrap_or(wildcard);
            (coding, q)
        });
    preferred(effective)
}

/// True when the request's `Accept-Encoding` is unsatisfiable by
/// `identity`, `gzip` or `br` — exactly the case where the tower-http
/// compression layer would answer 406 Not Acceptable.
pub fn accept_encoding_unsatisfied(headers: &HeaderMap) -> bool {
    negotiated_encoding(headers).is_none()
}

/// True when the negotiation would pick `br` or `gzip` (the layer may
/// compress): used to decide whether the pre-compression byte counter is
/// needed at all.
pub fn accept_encoding_accepts_compression(headers: &HeaderMap) -> bool {
    matches!(
        negotiated_encoding(headers),
        Some(Accepted::Gzip) | Some(Accepted::Br)
    )
}

// ---------------------------------------------------------------------------
// Response-body byte accounting (EDG-6)
// ---------------------------------------------------------------------------

/// Marker extension carrying the pre-compression byte counter of a response
/// (inserted by `count_compression_input`, read by
/// `compression_request_scope` after the compression layer — response
/// extensions survive the tower-http layer, which rebuilds the response from
/// `parts`).
#[derive(Debug, Clone)]
pub(crate) struct InBytes(pub(crate) Arc<AtomicU64>);

/// Body wrapper counting the bytes that pass through it (pre-compression,
/// before the tower-http layer consumes the body). It is a `http_body::Body`
/// — NOT a `Stream` — and DELEGATES `size_hint`/`is_end_stream` to the
/// original body: a body with a known exact size (the `Body::from(bytes)` of
/// a static asset or a pipeline error) keeps telling the layer's `SizeAbove`
/// predicate that size instead of looking like an unknown-size stream and
/// being compressed against the configured floor. Every frame is forwarded
/// as-is and the bytes are never copied; an abandoned body simply stops
/// counting — the counter keeps whatever passed.
struct CountingBody {
    inner: axum::body::Body,
    counter: Arc<AtomicU64>,
}

impl http_body::Body for CountingBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        match ready!(Pin::new(&mut self.inner).poll_frame(cx)) {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    self.counter.fetch_add(data.len() as u64, Ordering::Relaxed);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Err(err)) => Poll::Ready(Some(Err(err))),
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Stream wrapper over the FINAL (post-compression) body: counts the
/// compressed bytes and, when the stream is dropped — after a clean end,
/// an error, or a client disconnect — records the pre/post-compression
/// totals to `CompressionMetrics` exactly once (Rust drops a value
/// exactly once, so the record fires exactly once; an abandoned body
/// records whatever passed).
struct RecordingStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    out: AtomicU64,
    in_bytes: Arc<AtomicU64>,
    metrics: CompressionMetrics,
    encoding: CompressionEncoding,
}

impl Stream for RecordingStream {
    type Item = Result<Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let frame = match ready!(self.inner.as_mut().poll_next(cx)) {
            Some(frame) => frame,
            None => return Poll::Ready(None),
        };
        if let Ok(chunk) = &frame {
            self.out.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
        Poll::Ready(Some(frame))
    }
}

impl Drop for RecordingStream {
    fn drop(&mut self) {
        self.metrics.add(
            self.encoding,
            self.in_bytes.load(Ordering::Relaxed),
            self.out.load(Ordering::Relaxed),
        );
    }
}

/// True when some `Vary` field lists `accept-encoding` (case-insensitive
/// substring, the same check the tower-http 406 path applies before
/// appending the header).
fn vary_announces_accept_encoding(headers: &HeaderMap) -> bool {
    const NEEDLE: &[u8] = b"accept-encoding";
    headers.get_all(header::VARY).into_iter().any(|value| {
        let bytes = value.as_bytes();
        bytes.len() >= NEEDLE.len()
            && bytes
                .windows(NEEDLE.len())
                .any(|window| window.eq_ignore_ascii_case(NEEDLE))
    })
}

/// Middleware, mounted immediately INSIDE the compression layer (between it
/// and `owned_host_middleware`), that prepares the response for the layer
/// and counts the pre-compression bytes of the response body into an
/// `InBytes` extension.
///
/// HEAD responses are marked `HeadResponse` so the layer's predicate skips
/// them: HEAD is never compressed and its metadata passes through untouched
/// — the `content-length` the worker/pipeline sent (the size of the GET) is
/// preserved, and a 304 (which carries none) gains none. No header is
/// rewritten.
///
/// Every other response whose request could be compressed (`br`/`gzip`
/// accepted with q > 0 in the negotiation) is wrapped in a `CountingBody`
/// that preserves `size_hint`/`is_end_stream` and counts the data-frame
/// bytes as they pass; a response that reaches the layer WITHOUT a
/// `content-encoding` is additionally marked `ResponseWithoutEncoding`, so
/// the scope middleware can tell after the layer that it was the layer that
/// added the final encoding — covering both the Ok and the Err branches of
/// `pipeline_handler`. Every other response passes through untouched. The
/// counter survives the compression layer as a response extension.
pub(crate) async fn count_compression_input(req: Request, next: Next) -> Response {
    if req.method() == axum::http::Method::HEAD {
        let mut res = next.run(req).await;
        mark_head_response(&mut res);
        return res;
    }
    if !accept_encoding_accepts_compression(req.headers()) {
        return next.run(req).await;
    }
    let counter = Arc::new(AtomicU64::new(0));
    let mut res = next.run(req).await;
    if !res.headers().contains_key(header::CONTENT_ENCODING) {
        mark_response_without_encoding(&mut res);
    }
    let (parts, body) = res.into_parts();
    let body = axum::body::Body::new(CountingBody {
        inner: body,
        counter: Arc::clone(&counter),
    });
    let mut res = Response::from_parts(parts, body);
    res.extensions_mut().insert(InBytes(counter));
    res
}

/// Middleware, mounted OUTSIDE the compression layer (and outside
/// `weaken_worker_etag`), that scopes the tower-http 406 to the data plane
/// and closes the compression byte accounting.
///
/// Inbound: when the `Accept-Encoding` is unsatisfiable (no `br`, `gzip`
/// or `identity` accepted — the tower-http 406 case), the header is
/// REMOVED so the layer negotiates identity and stops answering 406 on its
/// own.
///
/// Outbound, for an unsatisfiable request: an app response (`AppResponse`
/// marker) is re-answered 406 Not Acceptable with the SAME body/headers the
/// tower-http layer would pass through today (only the status is
/// overwritten and `Vary: Accept-Encoding` appended when missing); the
/// control plane (health/ready/metrics/MCP/admin) returns its normal
/// response, uncompressed, and never answers 406.
///
/// Outbound, otherwise: when the layer compressed the response — an app
/// response that reached the layer WITHOUT a `content-encoding` (the
/// `ResponseWithoutEncoding` marker inserted by the inner byte-counting
/// middleware, which covers both the Ok and the Err branches of
/// `pipeline_handler`) now carrying a final `content-encoding` of `br` or
/// `gzip` — the final body is wrapped in a `RecordingStream` that records
/// the pre/post-compression byte totals to `CompressionMetrics` on drop.
/// Worker-provided encodings (present before the layer, hence unmarked) pass
/// through uncounted.
pub(crate) async fn compression_request_scope(
    State(state): State<OrchestratorState>,
    mut req: Request,
    next: Next,
) -> Response {
    let unsatisfied = accept_encoding_unsatisfied(req.headers());
    if unsatisfied {
        req.headers_mut().remove(header::ACCEPT_ENCODING);
    }
    let res = next.run(req).await;

    if unsatisfied {
        if res.extensions().get::<AppResponse>().is_some() {
            // The tower-http 406 passes the inner response through with the
            // status overwritten and Vary appended when missing; the header
            // removal above made the layer negotiate identity, so the body
            // and headers here are the plain (uncompressed) ones.
            let mut res = res;
            *res.status_mut() = StatusCode::NOT_ACCEPTABLE;
            if !vary_announces_accept_encoding(res.headers()) {
                res.headers_mut()
                    .append(header::VARY, HeaderValue::from_static("accept-encoding"));
            }
            return res;
        }
        return res;
    }

    let encoding = match res
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
    {
        Some(value) if value.eq_ignore_ascii_case("br") => Some(CompressionEncoding::Br),
        Some(value) if value.eq_ignore_ascii_case("gzip") => Some(CompressionEncoding::Gzip),
        _ => None,
    };
    let Some(encoding) = encoding else { return res };
    if res.extensions().get::<AppResponse>().is_none()
        || res.extensions().get::<ResponseWithoutEncoding>().is_none()
    {
        return res;
    }
    let in_bytes = res
        .extensions()
        .get::<InBytes>()
        .map(|in_bytes| Arc::clone(&in_bytes.0))
        .unwrap_or_default();
    let (parts, body) = res.into_parts();
    let body = axum::body::Body::from_stream(RecordingStream {
        inner: Box::pin(body.into_data_stream()),
        out: AtomicU64::new(0),
        in_bytes,
        metrics: state.server.compression_metrics(),
        encoding,
    });
    Response::from_parts(parts, body)
}

/// Middleware, mounted as the OUTERMOST layer of `build_pipeline` (unconditional
/// — it is not a compression feature): a 304 carries no representation data
/// (RFC 9110 §15.4.5), so remove any `content-length` header and, decisively,
/// leave the response body as an UNKNOWN-size empty stream.
///
/// Why the body swap: axum's `RouteFuture` post-processing
/// (`set_content_length`, `axum/routing/route.rs`) records
/// `size_hint().exact()` as `content-length` whenever the header is absent,
/// and it runs around the router's layers; on the catch-all-fallback data
/// plane the HEAD body is already the exact-0 `Body::empty()` by the time the
/// layers see it, so the stamp records `content-length: 0` on a HEAD 304
/// (observed since the EDG-6 rework; main `0154d3a` returns NO
/// `content-length` on a 304 — verified on a base copy, this correction). An
/// unknown-size empty stream — the same shape `pipeline.rs` already gives a
/// GET 304 (`pipeline.rs`, RFC 9110 §15.4.5) — has no exact size for the
/// stamp to record, so a 304 leaves with no `content-length` at all: for GET
/// and HEAD, apps and control plane. The stream yields no bytes: a 304 body
/// is empty by definition.
pub(crate) async fn strip_304_content_length(req: Request, next: Next) -> Response {
    let res = next.run(req).await;
    if res.status() != StatusCode::NOT_MODIFIED {
        return res;
    }
    let mut res = res;
    res.headers_mut().remove(header::CONTENT_LENGTH);
    res.map(|_| {
        axum::body::Body::from_stream(futures_util::stream::empty::<
            std::result::Result<Bytes, axum::Error>,
        >())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Response as HttpResponse;
    use axum::http::{HeaderName, HeaderValue, StatusCode};
    use bytes::Bytes;
    use futures_util::stream;

    const TYPE_HTML: &str = "content-type";
    const CACHE_CONTROL: &str = "cache-control";
    const CONTENT_DISPOSITION: &str = "content-disposition";

    fn body_of(size: usize) -> Body {
        Body::from(vec![b'a'; size])
    }

    /// A marked, 200, `text/html` response with `extra` headers — the
    /// positive control every table row is compared against.
    fn marked(extra: &[(&str, &str)]) -> HttpResponse<Body> {
        let mut res = HttpResponse::new(body_of(2048));
        res.headers_mut().insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("text/html"),
        );
        for (name, value) in extra {
            res.headers_mut().insert(
                name.parse::<HeaderName>().unwrap(),
                value.parse::<HeaderValue>().unwrap(),
            );
        }
        res.extensions_mut().insert(AppResponse);
        res
    }

    fn compresses(res: &HttpResponse<Body>) -> bool {
        app_response_predicate().should_compress(res)
    }

    #[test]
    fn unmarked_response_is_never_compressed() {
        let mut res = HttpResponse::new(body_of(2048));
        res.headers_mut().insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("text/html"),
        );
        assert!(!compresses(&res), "control plane response must stay plain");
    }

    #[test]
    fn marked_text_body_above_floor_is_compressed() {
        assert!(compresses(&marked(&[])));
    }

    #[test]
    fn no_transform_in_cache_control_is_skipped() {
        assert!(!compresses(&marked(&[(CACHE_CONTROL, "no-transform")])));
        assert!(!compresses(&marked(&[(
            CACHE_CONTROL,
            "public, no-transform, max-age=60"
        )])));
        assert!(!compresses(&marked(&[(
            CACHE_CONTROL,
            "max-age=300, NO-TRANSFORM"
        )])));
        assert!(!compresses(&marked(&[(
            CACHE_CONTROL,
            "no-transform; foo=bar"
        )])));
        // Other directives must not block compression.
        assert!(compresses(&marked(&[(
            CACHE_CONTROL,
            "no-cache, max-age=300"
        )])));
    }

    #[test]
    fn precompressed_content_types_are_skipped() {
        for content_type in [
            "image/png",
            "video/mp4",
            "audio/mpeg",
            "font/woff",
            "font/woff2",
            "application/zip",
            "application/gzip",
            "application/x-gzip",
            "application/x-brotli",
            "application/zstd",
            "application/pdf",
            "application/octet-stream",
        ] {
            assert!(
                !compresses(&marked(&[(TYPE_HTML, content_type)])),
                "{content_type} must be skipped"
            );
        }
        // Parameters do not defeat the match.
        assert!(!compresses(&marked(&[(
            TYPE_HTML,
            "application/zip; name=app.zip"
        )])));
        // svg and plain text/JSON stay compressible.
        assert!(compresses(&marked(&[(TYPE_HTML, "image/svg+xml")])));
        assert!(compresses(&marked(&[(
            TYPE_HTML,
            "application/javascript; charset=utf-8"
        )])));
        assert!(compresses(&marked(&[(TYPE_HTML, "font/ttf")])));
    }

    #[test]
    fn attachment_disposition_is_skipped() {
        assert!(!compresses(&marked(&[(CONTENT_DISPOSITION, "attachment")])));
        assert!(!compresses(&marked(&[(
            CONTENT_DISPOSITION,
            "attachment; filename=\"report.pdf\""
        )])));
        assert!(!compresses(&marked(&[(CONTENT_DISPOSITION, "ATTACHMENT")])));
        assert!(compresses(&marked(&[(CONTENT_DISPOSITION, "inline")])));
        assert!(compresses(&marked(&[])));
    }

    #[test]
    fn bodyless_statuses_are_skipped() {
        for status in [
            StatusCode::CONTINUE,
            StatusCode::from_u16(199).unwrap(),
            StatusCode::NO_CONTENT,
            StatusCode::NOT_MODIFIED,
        ] {
            let mut res = marked(&[]);
            *res.status_mut() = status;
            assert!(!compresses(&res), "{status:?} must be skipped");
        }
        for status in [
            StatusCode::OK,
            StatusCode::CREATED,
            StatusCode::MOVED_PERMANENTLY,
            StatusCode::NOT_FOUND,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let mut res = marked(&[]);
            *res.status_mut() = status;
            assert!(compresses(&res), "{status:?} must stay compressible");
        }
    }

    #[test]
    fn known_size_below_the_1024_floor_is_skipped() {
        let small = small_with_marker(HttpResponse::new(body_of(
            MIN_COMPRESSIBLE_BYTES as usize - 1,
        )));
        assert!(!app_response_predicate().should_compress(&small));

        let at_floor =
            small_with_marker(HttpResponse::new(body_of(MIN_COMPRESSIBLE_BYTES as usize)));
        assert!(app_response_predicate().should_compress(&at_floor));
    }

    fn small_with_marker(mut res: HttpResponse<Body>) -> HttpResponse<Body> {
        res.headers_mut().insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("text/html"),
        );
        res.extensions_mut().insert(AppResponse);
        res
    }

    fn stream_body_of(size: usize) -> Body {
        Body::from_stream(stream::iter(vec![Ok::<Bytes, std::io::Error>(
            Bytes::from(vec![b'a'; size]),
        )]))
    }

    #[test]
    fn unknown_size_body_is_compressed_and_content_length_header_is_honored() {
        // Streaming body: no size hint, no content-length → compressible.
        let res = small_with_marker(HttpResponse::new(stream_body_of(2048)));
        assert!(compresses(&res));

        // Streaming body whose content-length says below the floor.
        let mut below = small_with_marker(HttpResponse::new(stream_body_of(2048)));
        below.headers_mut().insert(
            HeaderName::from_static("content-length"),
            HeaderValue::from_static("1023"),
        );
        assert!(!compresses(&below));

        let mut at = small_with_marker(HttpResponse::new(stream_body_of(2048)));
        at.headers_mut().insert(
            HeaderName::from_static("content-length"),
            HeaderValue::from_static("1024"),
        );
        assert!(compresses(&at));
    }

    // --- EDG-6 corrections: HEAD marker + size-preserving counting body ----

    /// A no-op waker: the bodies consumed here (exact-size and finite
    /// iterator streams) never pend.
    struct NoopWake;

    impl std::task::Wake for NoopWake {
        fn wake(self: std::sync::Arc<Self>) {}
    }

    /// Consume every frame of a body with a no-op waker. The bodies wrapped
    /// here (exact-size and finite-iterator) never pend.
    fn drain_frames<B>(body: &mut B) -> Vec<Bytes>
    where
        B: http_body::Body<Data = Bytes> + Unpin,
        B::Error: std::fmt::Display,
    {
        let waker = std::task::Waker::from(std::sync::Arc::new(NoopWake));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut data = Vec::new();
        loop {
            match Pin::new(&mut *body).poll_frame(&mut cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    data.push(frame.into_data().expect("a data frame"));
                }
                Poll::Ready(Some(Err(err))) => panic!("body error: {err}"),
                Poll::Ready(None) => return data,
                Poll::Pending => panic!("body pended (unexpected for exact/finite bodies)"),
            }
        }
    }

    #[test]
    fn compression_predicate_skips_the_head_marker() {
        // Control: the same body marked as an app response compresses.
        let mut res = marked(&[]);
        assert!(compresses(&res));
        // The HEAD marker skips it even though every other rule allows it
        // (large `text/html` body, 200).
        res.extensions_mut().insert(HeadResponse);
        assert!(!compresses(&res));
    }

    #[test]
    fn compression_counting_body_preserves_size_hint_and_counts_bytes() {
        let original = axum::body::Body::from(vec![b'a'; 2048]);
        let hint = original.size_hint();
        let end_stream = original.is_end_stream();
        let counter = Arc::new(AtomicU64::new(0));
        let mut body = axum::body::Body::new(CountingBody {
            inner: original,
            counter: Arc::clone(&counter),
        });
        // The exact size of the original body survives the wrapper — the
        // layer's SizeAbove predicate must see it (the regression a
        // `Body::from_stream` wrapper erases).
        assert_eq!(
            body.size_hint().lower(),
            hint.lower(),
            "the lower bound is delegated"
        );
        assert_eq!(
            body.size_hint().upper(),
            hint.upper(),
            "the upper bound is delegated"
        );
        assert_eq!(body.size_hint().exact(), Some(2048));
        assert_eq!(
            body.is_end_stream(),
            end_stream,
            "is_end_stream is delegated"
        );
        let data = drain_frames(&mut body);
        let total: usize = data.iter().map(|chunk| chunk.len()).sum();
        assert_eq!(total, 2048, "every byte passes through");
        assert_eq!(data.len(), 1, "one frame for an exact-size body");
        assert_eq!(
            counter.load(Ordering::Relaxed),
            2048,
            "the data-frame bytes are counted"
        );
    }

    #[test]
    fn compression_counting_body_keeps_unknown_size_and_counts_frames() {
        let stream = stream::iter(vec![
            Ok::<Bytes, axum::Error>(Bytes::from(vec![b'x'; 1000])),
            Ok(Bytes::from(vec![b'y'; 512])),
        ]);
        let original = axum::body::Body::from_stream(stream);
        let hint = original.size_hint();
        let counter = Arc::new(AtomicU64::new(0));
        let mut body = axum::body::Body::new(CountingBody {
            inner: original,
            counter: Arc::clone(&counter),
        });
        // An unknown size stays unknown (delegated, not invented).
        assert_eq!(
            body.size_hint().lower(),
            hint.lower(),
            "the lower bound is delegated"
        );
        assert_eq!(
            body.size_hint().upper(),
            hint.upper(),
            "the upper bound is delegated"
        );
        assert_eq!(body.size_hint().exact(), None);
        let data = drain_frames(&mut body);
        let total: usize = data.iter().map(|chunk| chunk.len()).sum();
        assert_eq!(total, 1512, "every byte passes through");
        assert_eq!(data.len(), 2, "two frames, forwarded as-is");
        assert_eq!(
            counter.load(Ordering::Relaxed),
            1512,
            "the data-frame bytes are counted across frames"
        );
    }

    // Discriminating case for the `CountingBody` wiring (mutation guard):
    // through the REAL middleware + layer wiring (`count_compression_input`
    // + `compression_layer_with_config`), a body below the configured floor
    // with NO `content-length` header must stay plain — the layer's
    // `SizeAbove` predicate finds no size in the headers and can only rely
    // on the body's `size_hint`, which the wrapper must delegate. The
    // pre-fix `Body::from_stream` wrapper presents an unknown size there and
    // `SizeAbove` treats unknown as compressible, so the below-floor body
    // WOULD be compressed (this test fails on that wiring). In the full
    // pipeline this exact case is masked: axum's catch-all-fallback
    // top-level post-processing stamps `content-length` on exact-size
    // bodies before the stack runs, and `SizeAbove` falls back to the
    // header. A plain route (no catch-all fallback) keeps the header away
    // from the layer.
    #[tokio::test]
    async fn below_floor_body_without_content_length_stays_plain_through_the_layer() {
        use tower::ServiceExt;

        let app = axum::Router::new()
            .route(
                "/small",
                axum::routing::get(|| async {
                    let mut res = Response::new(Body::from(vec![b'x'; 1024]));
                    res.headers_mut().insert(
                        HeaderName::from_static("content-type"),
                        HeaderValue::from_static("text/html"),
                    );
                    mark_app_response(&mut res);
                    res
                }),
            )
            .route(
                "/large",
                axum::routing::get(|| async {
                    let mut res = Response::new(Body::from(vec![b'y'; 8192]));
                    res.headers_mut().insert(
                        HeaderName::from_static("content-type"),
                        HeaderValue::from_static("text/html"),
                    );
                    mark_app_response(&mut res);
                    res
                }),
            )
            .layer(axum::middleware::from_fn(count_compression_input))
            .layer(compression_layer_with_config(&CompressionConfig {
                min_bytes: 4096,
                ..Default::default()
            }));

        let request = |path: &str| {
            axum::http::Request::builder()
                .uri(path)
                .header("accept-encoding", "br")
                .body(Body::empty())
                .unwrap()
        };

        // 1024 B < 4096 B floor: plain — the wrapper's delegated size_hint
        // is the only size signal the predicate has.
        let res = app.clone().oneshot(request("/small")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(
            !res.headers()
                .contains_key(axum::http::header::CONTENT_ENCODING),
            "the below-floor body must not be compressed"
        );
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body.len(), 1024);

        // 8192 B >= floor: compressed (control: the floor still applies).
        let res = app.oneshot(request("/large")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()
                .get(axum::http::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("br"),
            "the above-floor body must be compressed"
        );
    }

    #[test]
    fn default_predicate_rules_are_kept() {
        assert!(!compresses(&marked(&[(TYPE_HTML, "text/event-stream")])));
        assert!(!compresses(&marked(&[(TYPE_HTML, "application/grpc")])));
        assert!(!compresses(&marked(&[(TYPE_HTML, "image/png")])));
        assert!(compresses(&marked(&[(TYPE_HTML, "image/svg+xml")])));
    }

    #[test]
    fn weaken_etag_downgrades_strong_and_keeps_weak() {
        // A comma is a valid character inside the quotes: the ETag field is a
        // SINGLE opaque value and is never split.
        assert_eq!(weaken_etag(b"\"a,b\""), Some(b"W/\"a,b\"".to_vec()));
        // Already weak (comma inside the weak tag): unchanged.
        assert_eq!(weaken_etag(b"W/\"a,b\""), None);
        assert_eq!(weaken_etag(b"\"v1\""), Some(b"W/\"v1\"".to_vec()));
        assert_eq!(weaken_etag(b"W/\"v1\""), None);
        // Opaque tag with special characters: all bytes preserved.
        assert_eq!(weaken_etag(b"\"a!b@c#d\""), Some(b"W/\"a!b@c#d\"".to_vec()));
    }

    #[test]
    fn weaken_etag_preserves_obs_text_bytes() {
        // obs-text (bytes >= 0x80) inside a strong tag: W/ + original bytes.
        let strong = b"\"\xc3\xa9\""; // "é" as UTF-8
        assert_eq!(weaken_etag(strong), Some(b"W/\"\xc3\xa9\"".to_vec()));
        // Already-weak obs-text: unchanged.
        assert_eq!(weaken_etag(b"W/\"\xc3\xa9\""), None);
    }

    #[test]
    fn weaken_etag_leaves_invalid_values_untouched() {
        // Empty, without quotes, an unclosed quote, or a lowercase `w/`
        // (not the normative weak prefix): invalid entity-tags are neither
        // weakened nor removed.
        assert_eq!(weaken_etag(b""), None);
        assert_eq!(weaken_etag(b"abc"), None);
        assert_eq!(weaken_etag(b"\"unclosed"), None);
        assert_eq!(weaken_etag(b"\""), None);
        assert_eq!(weaken_etag(b"w/\"a\""), None);
    }

    // --- EDG-6: configuration parsing ---------------------------------------

    #[test]
    fn compression_enabled_parses_on_and_off() {
        assert_eq!(CompressionConfig::parse_enabled("on"), Some(true));
        assert_eq!(CompressionConfig::parse_enabled("off"), Some(false));
        // Case-insensitive, trimmed.
        assert_eq!(CompressionConfig::parse_enabled(" ON "), Some(true));
        assert_eq!(CompressionConfig::parse_enabled("Off"), Some(false));
        // Anything else is invalid (the caller keeps the default).
        for invalid in ["", " ", "maybe", "1", "0", "true", "false", "onoff"] {
            assert_eq!(
                CompressionConfig::parse_enabled(invalid),
                None,
                "{invalid:?} must be invalid"
            );
        }
    }

    #[test]
    fn compression_level_parses_named_and_precise() {
        assert_eq!(
            CompressionConfig::parse_level("default"),
            Some(CompressionLevel::Default)
        );
        assert_eq!(
            CompressionConfig::parse_level("Fastest"),
            Some(CompressionLevel::Fastest)
        );
        assert_eq!(
            CompressionConfig::parse_level("best"),
            Some(CompressionLevel::Best)
        );
        // Integers map to Precise; 0 is a valid precise level.
        assert_eq!(
            CompressionConfig::parse_level("0"),
            Some(CompressionLevel::Precise(0))
        );
        assert_eq!(
            CompressionConfig::parse_level(" 11 "),
            Some(CompressionLevel::Precise(11))
        );
    }

    #[test]
    fn compression_level_rejects_invalid_values() {
        for invalid in [
            "",
            " ",
            "maximum",
            "-1",
            "+3",
            "3.5",
            "11x",
            "99999999999999999999",
            "default=1",
        ] {
            assert_eq!(
                CompressionConfig::parse_level(invalid),
                None,
                "{invalid:?} must be invalid"
            );
        }
    }

    #[test]
    fn compression_config_default_is_on_1024_default_level() {
        let config = CompressionConfig::default();
        assert!(config.enabled);
        assert_eq!(config.min_bytes, MIN_COMPRESSIBLE_BYTES);
        assert_eq!(config.level, CompressionLevel::Default);
    }

    // --- EDG-6: Accept-Encoding negotiation mirror ---------------------------
    //
    // These cases mirror the tower-http 0.7 `content_encoding` test suite,
    // adapted to the layer's supported set {identity, gzip, br} (deflate and
    // zstd are not enabled for this layer and are ignored, like in the
    // tower-http `Encoding::parse` with the matching feature set).

    fn accept_headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::ACCEPT_ENCODING, value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn negotiation_no_header_is_identity() {
        assert_eq!(
            negotiated_encoding(&accept_headers(&[])),
            Some(Accepted::Identity)
        );
    }

    #[test]
    fn negotiation_picks_best_accepted_coding() {
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip"])),
            Some(Accepted::Gzip)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip,br"])),
            Some(Accepted::Br)
        );
        // x-gzip is an alias of gzip (same as tower-http).
        assert_eq!(
            negotiated_encoding(&accept_headers(&["x-gzip"])),
            Some(Accepted::Gzip)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["deflate,x-gzip"])),
            Some(Accepted::Gzip)
        );
        // Unsupported codings (deflate/zstd) are ignored.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["deflate"])),
            Some(Accepted::Identity)
        );
        // Quality order: higher q wins; a tie goes to the higher-priority
        // coding (br > gzip), exactly as the tower-http enum order.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip;q=0.5,br"])),
            Some(Accepted::Br)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip;q=0.8,br;q=0.5"])),
            Some(Accepted::Gzip)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip;q=0.995,br;q=0.999"])),
            Some(Accepted::Br)
        );
        // Case-insensitive codings (and q parameter).
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gZiP"])),
            Some(Accepted::Gzip)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip;q=0.5,br;Q=0.8"])),
            Some(Accepted::Br)
        );
        // Allowed spaces around tokens and parameters.
        assert_eq!(
            negotiated_encoding(&accept_headers(&[" gzip\t; q=0.5 ,\tbr ;\tq=0.8\t"])),
            Some(Accepted::Br)
        );
        // Repeated ACCEPT_ENCODING fields are all considered.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip;q=0.5", "br"])),
            Some(Accepted::Br)
        );
    }

    #[test]
    fn negotiation_quality_zero_falls_back_to_identity() {
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip;q=0"])),
            Some(Accepted::Identity)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["br;q=0"])),
            Some(Accepted::Identity)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip;q=0,br;q=0"])),
            Some(Accepted::Identity),
            "identity is not explicitly rejected: the plain body is still acceptable"
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["gzip;q=0,br;q=0.5"])),
            Some(Accepted::Br)
        );
    }

    #[test]
    fn negotiation_unsatisfiable_cases_return_none() {
        // The tower-http 406 cases: identity explicitly rejected with
        // nothing acceptable.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["identity;q=0"])),
            None
        );
        assert_eq!(negotiated_encoding(&accept_headers(&["*;q=0"])), None);
        assert_eq!(
            negotiated_encoding(&accept_headers(&["*;q=0,identity;q=0"])),
            None
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["br;q=0,gzip;q=0,identity;q=0"])),
            None
        );
    }

    #[test]
    fn negotiation_wildcard_covers_unlisted_codings() {
        // `*` accepts every supported coding at the wildcard q.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["*"])),
            Some(Accepted::Br)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["*;q=0,gzip"])),
            Some(Accepted::Gzip)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["*;q=0,identity"])),
            Some(Accepted::Identity)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["identity;q=0,gzip"])),
            Some(Accepted::Gzip)
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["*;q=0.5,gzip;q=1"])),
            Some(Accepted::Gzip)
        );
        // br listed at q=0 while the wildcard covers gzip.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["br;q=0,*"])),
            Some(Accepted::Gzip)
        );
    }

    #[test]
    fn negotiation_invalid_qvalue_drops_the_entry() {
        // Invalid q-values drop the whole entry (tower-http rule); with only
        // invalid entries left, identity (unrejected) is the answer.
        for invalid in [
            "gzip;q =0.5",
            "gzip;q= 0.5",
            "gzip;q=-0.1",
            "gzip;q=00.5",
            "gzip;q=0.5000",
            "gzip;q=.5",
            "gzip;q=1.01",
            "gzip;q=1.001",
            "gzip;q=2",
        ] {
            assert_eq!(
                negotiated_encoding(&accept_headers(&[invalid])),
                Some(Accepted::Identity),
                "{invalid:?}"
            );
        }
        // Unknown coding with a valid gzip entry: the unknown one is ignored.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["invalid,gzip"])),
            Some(Accepted::Gzip)
        );
        // A `*` with an invalid q is skipped, not fatal.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["*;q=bad,gzip"])),
            Some(Accepted::Gzip)
        );
    }

    #[test]
    fn negotiation_first_wildcard_and_first_explicit_entry_win() {
        // First `*` entry wins (tower-http `find_map` rule).
        assert_eq!(
            negotiated_encoding(&accept_headers(&["*;q=0,*;q=0.5"])),
            None,
            "the first wildcard (q=0) decides: nothing is acceptable"
        );
        assert_eq!(
            negotiated_encoding(&accept_headers(&["*;q=bad,*;q=0.5"])),
            Some(Accepted::Br),
            "the first wildcard with an invalid q is skipped, the second wins"
        );
        // First EXPLICIT entry of a coding wins in the wildcard branch
        // (tower-http `find` rule): br listed at 0 then 0.9 still takes 0
        // against the wildcard.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["br;q=0,br;q=0.9,*"])),
            Some(Accepted::Gzip),
            "effective br q=0 (first entry) loses to the wildcard-covered gzip"
        );
        // Without the wildcard the max over ALL explicit entries wins.
        assert_eq!(
            negotiated_encoding(&accept_headers(&["br;q=0,br;q=0.9"])),
            Some(Accepted::Br)
        );
    }

    #[test]
    fn accept_encoding_unsatisfied_and_accepts_compression_flags() {
        assert!(accept_encoding_unsatisfied(&accept_headers(&[
            "identity;q=0"
        ])));
        assert!(accept_encoding_unsatisfied(&accept_headers(&["*;q=0"])));
        assert!(!accept_encoding_unsatisfied(&accept_headers(&[
            "br;q=0,gzip;q=0"
        ])));
        assert!(!accept_encoding_unsatisfied(&accept_headers(&[])));

        assert!(accept_encoding_accepts_compression(&accept_headers(&[
            "br"
        ])));
        assert!(accept_encoding_accepts_compression(&accept_headers(&[
            "gzip"
        ])));
        assert!(accept_encoding_accepts_compression(&accept_headers(&["*"])));
        assert!(accept_encoding_accepts_compression(&accept_headers(&[
            "*;q=0.1"
        ])));
        assert!(!accept_encoding_accepts_compression(&accept_headers(&[])));
        assert!(!accept_encoding_accepts_compression(&accept_headers(&[
            "identity"
        ])));
        assert!(!accept_encoding_accepts_compression(&accept_headers(&[
            "br;q=0"
        ])));
        assert!(!accept_encoding_accepts_compression(&accept_headers(&[
            "identity;q=0"
        ])));
    }
}
