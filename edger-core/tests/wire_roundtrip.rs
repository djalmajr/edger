//! Wire format roundtrip tests (story 02.03).

use bytes::Bytes;
use edger_core::{validate_headers, SerializedRequest, SerializedResponse, MAX_HEADER_VALUE_BYTES};

#[test]
fn serialized_request_roundtrips_with_body() {
    let req = SerializedRequest {
        method: "POST".into(),
        uri: "/api/users?x=1".into(),
        headers: vec![("content-type".into(), "application/json".into())],
        body: Some(Bytes::from_static(b"{\"ok\":true}")),
        request_id: "req-1".into(),
        base_href: Some("/@acme/app/".into()),
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: SerializedRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, req);
}

#[test]
fn empty_body_serializes_as_null() {
    let req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "req-2".into(),
        base_href: None,
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"body\":null"));
    let back: SerializedRequest = serde_json::from_str(&json).unwrap();
    assert!(back.body.is_none());
}

#[test]
fn missing_required_field_fails_deserialize() {
    let bad = r#"{"uri":"/","headers":[]}"#;
    let err = serde_json::from_str::<SerializedRequest>(bad).unwrap_err();
    assert!(err.to_string().contains("missing field"));
}

#[test]
fn response_roundtrip_binary_safe_headers() {
    let res = SerializedResponse {
        status: 200,
        headers: vec![("x-custom".into(), "value-with-\u{00e9}".into())],
        body: Some(Bytes::from_static(&[0, 255, 1])),
    };
    let json = serde_json::to_string(&res).unwrap();
    let back: SerializedResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(back, res);
}

#[test]
fn header_limits_enforced() {
    let headers: Vec<_> = (0..101).map(|i| (format!("h-{i}"), "v".into())).collect();
    let err = validate_headers(&headers).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");

    let big_value = "x".repeat(MAX_HEADER_VALUE_BYTES + 1);
    let err = validate_headers(&[("h".into(), big_value)]).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
}
