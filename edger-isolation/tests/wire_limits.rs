//! Wire validation and IPC framing tests (story 03.03).

use bytes::Bytes;
use edger_core::{parse_worker_config, SerializedRequest, WorkerManifest};
use edger_isolation::{decode_frame, encode_frame, validate_request};

fn sample_req(headers: Vec<(String, String)>, body: Option<Bytes>) -> SerializedRequest {
    SerializedRequest {
        method: "POST".into(),
        uri: "/".into(),
        headers,
        body,
        request_id: "w".into(),
        base_href: None,
    }
}

#[test]
fn reject_too_many_headers() {
    let headers: Vec<_> = (0..101).map(|i| (format!("h{i}"), "v".into())).collect();
    let err = validate_request(&sample_req(headers, None), &default_config()).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
}

#[test]
fn reject_oversized_header_value() {
    let big = "x".repeat(8 * 1024 + 1);
    let err = validate_request(
        &sample_req(vec![("h".into(), big)], None),
        &default_config(),
    )
    .unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
}

#[test]
fn reject_oversized_body() {
    let config = parse_worker_config(&WorkerManifest {
        name: "w".into(),
        max_body_size: Some("1024".into()),
        ..Default::default()
    });
    let body = Bytes::from(vec![0u8; 2048]);
    let err = validate_request(&sample_req(vec![], Some(body)), &config).unwrap_err();
    assert_eq!(err.code, "VALIDATION_ERROR");
}

#[test]
fn frame_roundtrip_preserves_binary_body() {
    let req = sample_req(vec![], Some(Bytes::from_static(&[0, 255, 1])));
    let frame = encode_frame(&req).unwrap();
    let back = decode_frame(&frame).unwrap();
    assert_eq!(back, req);
}

fn default_config() -> edger_core::WorkerConfig {
    parse_worker_config(&WorkerManifest {
        name: "w".into(),
        ..Default::default()
    })
}
