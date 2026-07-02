//! HTTP <-> `SerializedRequest`/`SerializedResponse` conversion (story 05.03).

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use edger_core::{validate_headers, CoreError, SerializedRequest, SerializedResponse};

/// Max request body bytes accepted by the orchestrator wire layer (stub limit).
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Convert an axum/hyper request into a `SerializedRequest`.
pub async fn axum_to_serialized(
    req: Request<Body>,
    request_id: String,
) -> Result<SerializedRequest, CoreError> {
    let (parts, body) = req.into_parts();
    let headers = header_pairs(&parts.headers)?;
    validate_headers(&headers).map_err(|err| CoreError::new("HEADER_TOO_LARGE", err.message))?;

    let body_bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| CoreError::new("PAYLOAD_TOO_LARGE", "request body exceeds limit"))?;

    let uri = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());

    Ok(SerializedRequest {
        method: parts.method.to_string(),
        uri,
        headers,
        body: if body_bytes.is_empty() {
            None
        } else {
            Some(body_bytes)
        },
        request_id,
        base_href: None,
    })
}

/// Convert isolate wire response into an HTTP response.
pub fn serialized_to_axum(res: SerializedResponse) -> Result<Response<Body>, CoreError> {
    let body = match res.body {
        Some(bytes) => Body::from(bytes),
        None => Body::empty(),
    };
    response_with_headers(res.status, &res.headers, body)
}

/// Streamed worker response -> axum response whose body forwards chunks to the
/// client as the worker produces them (story 16.D).
pub fn streamed_to_axum(res: edger_core::StreamedResponse) -> Result<Response<Body>, CoreError> {
    let status = res.status;
    let headers = res.headers;
    let body = Body::from_stream(res.body);
    response_with_headers(status, &headers, body)
}

fn response_with_headers(
    status: u16,
    header_pairs: &[(String, String)],
    body: Body,
) -> Result<Response<Body>, CoreError> {
    validate_headers(header_pairs)?;
    let mut builder =
        Response::builder()
            .status(StatusCode::from_u16(status).map_err(|_| {
                CoreError::validation("status", format!("invalid status {status}"))
            })?);
    if let Some(headers) = builder.headers_mut() {
        for (name, value) in header_pairs {
            headers.append(
                name.parse::<axum::http::HeaderName>().map_err(|_| {
                    CoreError::validation("headers", format!("invalid name {name}"))
                })?,
                HeaderValue::from_str(value).map_err(|_| {
                    CoreError::validation("headers", format!("invalid value for {name}"))
                })?,
            );
        }
    }
    builder
        .body(body)
        .map_err(|e| CoreError::new("RESPONSE_ERROR", e.to_string()))
}

fn header_pairs(headers: &HeaderMap) -> Result<Vec<(String, String)>, CoreError> {
    let mut pairs = Vec::new();
    for (name, value) in headers {
        let value = value
            .to_str()
            .map_err(|_| CoreError::new("HEADER_INVALID", "non-utf8 header value"))?;
        pairs.push((name.to_string(), value.to_string()));
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use bytes::Bytes;

    #[tokio::test]
    async fn roundtrip_preserves_method_path_query_headers_body() {
        let req = Request::builder()
            .method("POST")
            .uri("/foo/bar?x=1")
            .header("content-type", "text/plain")
            .body(Body::from("payload"))
            .unwrap();

        let serialized = axum_to_serialized(req, "req-1".into()).await.unwrap();
        assert_eq!(serialized.method, "POST");
        assert_eq!(serialized.uri, "/foo/bar?x=1");
        assert_eq!(
            serialized.body.as_ref().map(|b| b.as_ref()),
            Some(b"payload".as_ref())
        );
        assert!(serialized
            .headers
            .iter()
            .any(|(k, v)| k == "content-type" && v == "text/plain"));

        let http = serialized_to_axum(SerializedResponse {
            status: 201,
            headers: vec![("x-test".into(), "ok".into())],
            body: Some(Bytes::from("done")),
        })
        .unwrap();
        assert_eq!(http.status(), StatusCode::CREATED);
    }
}
