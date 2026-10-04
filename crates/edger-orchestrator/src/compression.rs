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
//! - minimum compressible size of 1024 bytes when the size is known
//!   (`SizeAbove(1024)` in place of the 32 default). Unknown-size (streaming)
//!   bodies are compressed — the encoder flushes per chunk.
//!
//! Responses that already carry `content-encoding` or `content-range` are
//! skipped by tower-http itself, before the predicate runs.
//!
//! A request whose `Accept-Encoding` accepts no supported encoding makes
//! tower-http answer 406 Not Acceptable (RFC 9110 §12.5.3); that behavior is
//! kept as-is and covered by tests.

use axum::extract::Request;
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode, Version};
use axum::middleware::Next;
use axum::response::Response;
use tower_http::compression::predicate::{DefaultPredicate, Predicate, SizeAbove};
use tower_http::compression::{CompressionLayer, CompressionLevel};

/// Minimum body size (bytes) for compression when the size is known.
pub const MIN_COMPRESSIBLE_BYTES: u64 = 1024;

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
/// quality, gated by the EDG-2 policy (`app_response_predicate`).
pub fn compression_layer() -> CompressionLayer<impl Predicate> {
    CompressionLayer::new()
        .quality(CompressionLevel::Default)
        .compress_when(app_response_predicate())
}

/// EDG-2 policy (decision 4): the tower-http `DefaultPredicate` combined with
/// the data-plane rules listed in the module docs.
pub fn app_response_predicate() -> impl Predicate {
    DefaultPredicate::new()
        .and(SizeAbove::new(MIN_COMPRESSIBLE_BYTES))
        .and(
            |status: StatusCode,
             _version: Version,
             headers: &HeaderMap,
             extensions: &Extensions| {
                is_marked_app_response(extensions)
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
}
