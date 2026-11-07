//! SSE directives a guest returns in its response headers (EDG-55 S3).
//!
//! Pure functions, no I/O. The guest never names a worker, namespace or
//! version: the scope comes from the host. Every directive is validated
//! before any delivery, and an error is a static value that never echoes
//! what the guest sent.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use edger_core::validate_headers;
use serde::Deserialize;

use crate::sse_broker::SseEvent;

/// Response header with which the guest authorizes a subscription.
pub const SSE_SUBSCRIBE_HEADER: &str = "x-edger-sse-subscribe";
/// Response header (1 to 8 per response) with which the guest publishes.
pub const SSE_PUBLISH_HEADER: &str = "x-edger-sse-publish";
/// Largest guest body accepted on a subscription execution; the body is
/// discarded.
pub const SSE_SUBSCRIBE_MAX_BODY_BYTES: usize = 64 * 1024;

const SSE_HEADER_PREFIX: &str = "x-edger-sse-";
const MAX_TOPICS_PER_SUBSCRIPTION: usize = 8;
const MAX_PUBLISH_DIRECTIVES: usize = 8;
const MAX_TOPIC_BYTES: usize = 128;
const MAX_EVENT_FIELD_BYTES: usize = 128;

/// A directive that breaks the contract. The message is static.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseDirectiveError {
    Invalid,
}

impl std::fmt::Display for SseDirectiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid SSE directive")
    }
}

impl std::error::Error for SseDirectiveError {}

/// The size limits of a response head that carries a directive: count, value
/// and total bytes, the same `edger_core::validate_headers` applies to
/// requests and to WASM heads. The Deno path does not check a response head
/// anywhere else, and checking every response would change ordinary
/// requests, so the host applies it only here. The error is static and never
/// echoes a header.
fn head_within_limits(headers: &[(String, String)]) -> Result<(), SseDirectiveError> {
    validate_headers(headers).map_err(|_| SseDirectiveError::Invalid)
}

/// `x-edger-sse-subscribe`: `Ok(None)` when absent, the topics when valid.
/// A repeated header (any case), an empty list, more than 8 topics or a
/// topic outside `[A-Za-z0-9._:-]{1,128}` is `Invalid`. With the header
/// present, a head over the header limits is `Invalid` too.
pub fn parse_subscribe(
    headers: &[(String, String)],
) -> Result<Option<Vec<String>>, SseDirectiveError> {
    let mut values = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(SSE_SUBSCRIBE_HEADER))
        .map(|(_, value)| value.as_str());
    let Some(value) = values.next() else {
        return Ok(None);
    };
    head_within_limits(headers)?;
    if values.next().is_some() {
        return Err(SseDirectiveError::Invalid);
    }
    let topics: Vec<String> = value
        .split(',')
        .map(|topic| topic.trim_matches([' ', '\t']))
        .map(str::to_owned)
        .collect();
    if topics.is_empty()
        || topics.len() > MAX_TOPICS_PER_SUBSCRIPTION
        || !topics.iter().all(|topic| is_valid_topic(topic))
    {
        return Err(SseDirectiveError::Invalid);
    }
    Ok(Some(topics))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishDirective {
    data: String,
    event: Option<String>,
    id: Option<String>,
    topic: String,
}

/// Every `x-edger-sse-publish` header (1 to 8), each base64url without
/// padding over a JSON object with unknown fields refused. All of them are
/// validated before any is returned: one invalid makes the whole result
/// `Invalid`. No header is an empty list. With at least one directive
/// present, a head over the header limits (100 headers, 8 KiB per value,
/// 64 KiB in total) is `Invalid` before any directive is read.
pub fn parse_publish(headers: &[(String, String)]) -> Result<Vec<SseEvent>, SseDirectiveError> {
    let values: Vec<&str> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(SSE_PUBLISH_HEADER))
        .map(|(_, value)| value.as_str())
        .collect();
    if values.is_empty() {
        return Ok(Vec::new());
    }
    head_within_limits(headers)?;
    if values.len() > MAX_PUBLISH_DIRECTIVES {
        return Err(SseDirectiveError::Invalid);
    }
    values.into_iter().map(parse_publish_value).collect()
}

fn parse_publish_value(value: &str) -> Result<SseEvent, SseDirectiveError> {
    let raw = URL_SAFE_NO_PAD
        .decode(value.trim_matches([' ', '\t']))
        .map_err(|_| SseDirectiveError::Invalid)?;
    // A derived struct deserializer also accepts a JSON array in field
    // order; the contract is an object, so the document must open with `{`
    // (JSON allows only whitespace before it). The map visitor then keeps
    // refusing unknown and duplicate fields.
    if raw.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(SseDirectiveError::Invalid);
    }
    let directive: PublishDirective =
        serde_json::from_slice(&raw).map_err(|_| SseDirectiveError::Invalid)?;
    if !is_valid_topic(&directive.topic)
        || directive.data.contains('\0')
        || !is_valid_event_field(directive.event.as_deref())
        || !is_valid_event_field(directive.id.as_deref())
    {
        return Err(SseDirectiveError::Invalid);
    }
    Ok(SseEvent {
        topic: directive.topic,
        event: directive.event,
        id: directive.id,
        data: directive.data,
    })
}

/// Remove every `x-edger-sse-*` header (any case), in place.
pub fn strip_sse_headers(headers: &mut Vec<(String, String)>) {
    headers.retain(|(name, _)| !is_sse_header_name(name));
}

/// Whether a header name belongs to the `x-edger-sse-*` family.
pub fn is_sse_header_name(name: &str) -> bool {
    name.len() >= SSE_HEADER_PREFIX.len()
        && name.as_bytes()[..SSE_HEADER_PREFIX.len()]
            .eq_ignore_ascii_case(SSE_HEADER_PREFIX.as_bytes())
}

fn is_valid_topic(topic: &str) -> bool {
    !topic.is_empty()
        && topic.len() <= MAX_TOPIC_BYTES
        && topic
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn is_valid_event_field(value: Option<&str>) -> bool {
    value.is_none_or(|value| {
        value.len() <= MAX_EVENT_FIELD_BYTES && !value.contains(['\r', '\n', '\0'])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, value: &str) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    fn encode(json: &str) -> String {
        URL_SAFE_NO_PAD.encode(json)
    }

    fn publish_header(json: &str) -> (String, String) {
        header(SSE_PUBLISH_HEADER, &encode(json))
    }

    /// A real directive whose header value is exactly `value_len` bytes
    /// (base64url over a JSON object padded with data).
    fn directive_of_value_len(value_len: usize) -> (String, String) {
        // `{"topic":"t","data":""}` is 23 bytes; 4 base64 chars per 3 bytes.
        assert_eq!(value_len % 4, 0);
        let json_len = value_len / 4 * 3;
        let data = "d".repeat(json_len - 23);
        let json = format!(r#"{{"topic":"t","data":"{data}"}}"#);
        assert_eq!(json.len(), json_len);
        let pair = publish_header(&json);
        assert_eq!(pair.1.len(), value_len);
        pair
    }

    fn pads(count: usize, value_len: usize) -> Vec<(String, String)> {
        (0..count)
            .map(|index| (format!("x-pad-{index}"), "p".repeat(value_len)))
            .collect()
    }

    #[test]
    fn publish_applies_the_header_limits_only_when_a_directive_is_present() {
        // Exactly 8192 bytes: the largest value of a real directive.
        let (name, value) = directive_of_value_len(8192);
        assert_eq!(value.len(), 8192);
        let at_limit = parse_publish(&[(name.clone(), value)]).expect("8192 bytes is accepted");
        assert_eq!(at_limit.len(), 1);
        assert_eq!(at_limit[0].data.len(), 8192 / 4 * 3 - 23);
        // One base64 group more (8196 bytes, a real directive too) is refused.
        let (name, over) = directive_of_value_len(8196);
        assert_eq!(
            parse_publish(&[(name, over)]),
            Err(SseDirectiveError::Invalid)
        );
        // The rule covers the whole head: another header at 8192 passes, at
        // 8193 does not.
        let (name, value) = directive_of_value_len(64);
        let directive = (name, value);
        let mut head = vec![directive.clone(), ("x-big".into(), "a".repeat(8192))];
        assert_eq!(parse_publish(&head).map(|events| events.len()), Ok(1));
        head[1].1.push('a');
        assert_eq!(parse_publish(&head), Err(SseDirectiveError::Invalid));
        // Without a directive the same big header is not refused.
        assert_eq!(
            parse_publish(&[("x-big".into(), "a".repeat(8193))]),
            Ok(vec![])
        );
    }

    #[test]
    fn publish_refuses_a_head_over_the_total_or_the_count_limit() {
        // 8 values of 8192 bytes: 8 * (19 + 8192) = 65688 > 64 KiB.
        let (name, value) = directive_of_value_len(8192);
        let eight: Vec<_> = (0..8).map(|_| (name.clone(), value.clone())).collect();
        assert_eq!(parse_publish(&eight), Err(SseDirectiveError::Invalid));
        // Controls: 7 of them (57 477 bytes) and 8 small ones are accepted.
        assert_eq!(parse_publish(&eight[..7]).map(|events| events.len()), Ok(7));
        let (name, small) = directive_of_value_len(4000);
        let small_eight: Vec<_> = (0..8).map(|_| (name.clone(), small.clone())).collect();
        assert_eq!(
            parse_publish(&small_eight).map(|events| events.len()),
            Ok(8)
        );
        // More than 100 headers with one directive present are refused ...
        let (name, value) = directive_of_value_len(64);
        let mut many = pads(100, 1);
        many.push((name.clone(), value.clone()));
        assert_eq!(parse_publish(&many), Err(SseDirectiveError::Invalid));
        // ... exactly 100 are accepted ...
        let mut hundred = pads(99, 1);
        hundred.push((name, value));
        assert_eq!(hundred.len(), 100);
        assert_eq!(parse_publish(&hundred).map(|events| events.len()), Ok(1));
        // ... and the same crowd without a directive is left alone.
        assert_eq!(parse_publish(&pads(150, 1)), Ok(vec![]));
        assert_eq!(parse_publish(&pads(9, 8192)), Ok(vec![]));
    }

    #[test]
    fn subscribe_applies_the_header_limits_only_when_the_header_is_present() {
        let topic = (SSE_SUBSCRIBE_HEADER.to_string(), "room.1".to_string());
        let ok = |head: &[(String, String)]| parse_subscribe(head);
        // Controls: the header alone, and with a big (8192) neighbor.
        assert_eq!(
            ok(std::slice::from_ref(&topic)),
            Ok(Some(vec!["room.1".to_string()]))
        );
        let mut head = vec![topic.clone(), ("x-big".into(), "a".repeat(8192))];
        assert_eq!(ok(&head), Ok(Some(vec!["room.1".to_string()])));
        // A value over 8 KiB, more than 100 headers, a total over 64 KiB.
        head[1].1.push('a');
        assert_eq!(ok(&head), Err(SseDirectiveError::Invalid));
        let mut many = pads(100, 1);
        many.push(topic.clone());
        assert_eq!(ok(&many), Err(SseDirectiveError::Invalid));
        let mut heavy = pads(8, 8192);
        heavy.push(topic.clone());
        assert_eq!(ok(&heavy), Err(SseDirectiveError::Invalid));
        // The same heads without the header are not refused.
        assert_eq!(ok(&head[1..]), Ok(None));
        assert_eq!(ok(&pads(150, 1)), Ok(None));
        assert_eq!(ok(&pads(9, 8192)), Ok(None));
    }

    #[test]
    fn subscribe_absent_is_none_and_unrelated_headers_are_ignored() {
        assert_eq!(parse_subscribe(&[]), Ok(None));
        assert_eq!(
            parse_subscribe(&[header("x-edger-sse-publish", "x"), header("x-other", "t")]),
            Ok(None)
        );
    }

    #[test]
    fn subscribe_accepts_one_to_eight_valid_topics_and_any_header_case() {
        assert_eq!(
            parse_subscribe(&[header("X-Edger-SSE-Subscribe", "room.1")]),
            Ok(Some(vec!["room.1".to_string()]))
        );
        assert_eq!(
            parse_subscribe(&[header(SSE_SUBSCRIBE_HEADER, "a, b:c ,D_e,f-g")]),
            Ok(Some(
                ["a", "b:c", "D_e", "f-g"].map(str::to_string).to_vec()
            ))
        );
        let eight = (0..8)
            .map(|n| format!("t{n}"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse_subscribe(&[header(SSE_SUBSCRIBE_HEADER, &eight)])
            .expect("eight topics")
            .is_some());
        let longest = "a".repeat(128);
        assert!(parse_subscribe(&[header(SSE_SUBSCRIBE_HEADER, &longest)]).is_ok());
    }

    #[test]
    fn subscribe_refuses_every_invalid_form() {
        let nine = (0..9)
            .map(|n| format!("t{n}"))
            .collect::<Vec<_>>()
            .join(",");
        let too_long = "a".repeat(129);
        for bad in [
            "",
            ",",
            "a,",
            ",a",
            "a,,b",
            " ",
            "a b",
            "a/b",
            "a;b",
            "ä",
            "a\u{0}b",
            "a\nb",
            "a%2Fb",
            "a*",
            nine.as_str(),
            too_long.as_str(),
        ] {
            assert_eq!(
                parse_subscribe(&[header(SSE_SUBSCRIBE_HEADER, bad)]),
                Err(SseDirectiveError::Invalid),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn subscribe_refuses_a_repeated_header_in_any_case() {
        assert_eq!(
            parse_subscribe(&[
                header(SSE_SUBSCRIBE_HEADER, "a"),
                header("X-Edger-Sse-Subscribe", "b"),
            ]),
            Err(SseDirectiveError::Invalid)
        );
    }

    #[test]
    fn publish_absent_is_an_empty_list() {
        assert_eq!(parse_publish(&[]), Ok(vec![]));
        assert_eq!(
            parse_publish(&[header(SSE_SUBSCRIBE_HEADER, "a")]),
            Ok(vec![])
        );
    }

    #[test]
    fn publish_decodes_the_full_event_in_header_order() {
        let events = parse_publish(&[
            publish_header(r#"{"topic":"room.1","data":"hi\nthere","event":"msg","id":"42"}"#),
            header("x-other", "ignored"),
            publish_header(r#"{"topic":"t:2","data":""}"#),
        ])
        .expect("valid");
        assert_eq!(
            events,
            vec![
                SseEvent {
                    topic: "room.1".into(),
                    event: Some("msg".into()),
                    id: Some("42".into()),
                    data: "hi\nthere".into(),
                },
                SseEvent {
                    topic: "t:2".into(),
                    event: None,
                    id: None,
                    data: String::new(),
                },
            ]
        );
    }

    #[test]
    fn publish_accepts_one_to_eight_headers_and_refuses_nine() {
        let ok = publish_header(r#"{"topic":"t","data":"x"}"#);
        assert_eq!(parse_publish(&vec![ok.clone(); 8]).expect("eight").len(), 8);
        assert_eq!(parse_publish(&vec![ok; 9]), Err(SseDirectiveError::Invalid));
    }

    #[test]
    fn publish_refuses_bad_encodings() {
        let good = encode(r#"{"topic":"t","data":"x"}"#);
        let standard = "eyJ0b3BpYyI6InQ_ZGF0YSI6IngifQ";
        for bad in [
            format!("{good}="),
            format!("{good}=="),
            good.replace('e', "+"),
            standard.replace('_', "/"),
            "not base64!".to_string(),
            "%%%".to_string(),
        ] {
            assert_eq!(
                parse_publish(&[header(SSE_PUBLISH_HEADER, &bad)]),
                Err(SseDirectiveError::Invalid),
                "{bad}"
            );
        }
        // Padding is refused even when the standard decoder would accept it.
        let padded =
            base64::engine::general_purpose::URL_SAFE.encode(r#"{"topic":"t","data":"xx"}"#);
        assert!(padded.ends_with('='));
        assert_eq!(
            parse_publish(&[header(SSE_PUBLISH_HEADER, &padded)]),
            Err(SseDirectiveError::Invalid)
        );
    }

    #[test]
    fn publish_refuses_bad_json_shapes_and_unknown_fields() {
        for bad in [
            "",
            "null",
            "[]",
            "\"x\"",
            "{",
            r#"{"topic":"t"}"#,
            r#"{"data":"x"}"#,
            r#"{"topic":"t","data":1}"#,
            r#"{"topic":1,"data":"x"}"#,
            r#"{"topic":"t","data":"x","extra":1}"#,
            r#"{"topic":"t","data":"x","worker":"other-app"}"#,
            r#"{"topic":"t","data":"x","namespace":"@a"}"#,
            r#"{"topic":"t","data":"x","topic":"u"}"#,
            r#"{"topic":"t","data":"x","event":1}"#,
        ] {
            assert_eq!(
                parse_publish(&[publish_header(bad)]),
                Err(SseDirectiveError::Invalid),
                "{bad}"
            );
        }
    }

    #[test]
    fn publish_requires_a_json_object_not_a_positional_array() {
        // Serde's derived struct also reads a sequence in field order
        // (data, event, id, topic); the contract asks for an object.
        let array = r#"["hello",null,null,"room.1"]"#;
        let full_array = r#"["hello","evt","7","room.1"]"#;
        let object = r#"{"topic":"room.1","data":"hello"}"#;

        for bad in [array, full_array, "  [\"hello\",null,null,\"room.1\"]"] {
            assert_eq!(
                parse_publish(&[publish_header(bad)]),
                Err(SseDirectiveError::Invalid),
                "{bad}"
            );
        }
        // After a valid directive in the same batch, and before one.
        assert_eq!(
            parse_publish(&[publish_header(object), publish_header(array)]),
            Err(SseDirectiveError::Invalid)
        );
        assert_eq!(
            parse_publish(&[publish_header(array), publish_header(object)]),
            Err(SseDirectiveError::Invalid)
        );
        // Control: the object form, with leading whitespace, still passes.
        let padded = format!(" \n\t{object}");
        let ok = parse_publish(&[publish_header(&padded)]).expect("object accepted");
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].topic, "room.1");
        assert_eq!(ok[0].data, "hello");
    }

    #[test]
    fn publish_refuses_bad_topic_data_event_and_id() {
        let long = "x".repeat(129);
        let exact = "x".repeat(128);
        for bad in [
            r#"{"topic":"","data":"x"}"#.to_string(),
            r#"{"topic":"a b","data":"x"}"#.to_string(),
            r#"{"topic":"a/b","data":"x"}"#.to_string(),
            format!(r#"{{"topic":"{long}","data":"x"}}"#),
            r#"{"topic":"t","data":"a\u0000b"}"#.to_string(),
            r#"{"topic":"t","data":"x","event":"a\nb"}"#.to_string(),
            r#"{"topic":"t","data":"x","event":"a\rb"}"#.to_string(),
            r#"{"topic":"t","data":"x","event":"a\u0000b"}"#.to_string(),
            r#"{"topic":"t","data":"x","id":"a\nb"}"#.to_string(),
            r#"{"topic":"t","data":"x","id":"a\u0000"}"#.to_string(),
            format!(r#"{{"topic":"t","data":"x","event":"{long}"}}"#),
            format!(r#"{{"topic":"t","data":"x","id":"{long}"}}"#),
        ] {
            assert_eq!(
                parse_publish(&[publish_header(&bad)]),
                Err(SseDirectiveError::Invalid),
                "{bad}"
            );
        }
        let ok = parse_publish(&[publish_header(&format!(
            r#"{{"topic":"{exact}","data":"x","event":"{exact}","id":"{exact}"}}"#
        ))]);
        assert_eq!(ok.expect("128 bytes is the limit").len(), 1);
        // Data keeps CR and LF: the broker turns them into data lines.
        assert!(parse_publish(&[publish_header(r#"{"topic":"t","data":"a\r\nb"}"#)]).is_ok());
    }

    #[test]
    fn one_invalid_directive_makes_the_whole_result_invalid_even_when_second() {
        let good = publish_header(r#"{"topic":"t","data":"x"}"#);
        let bad = publish_header(r#"{"topic":"t","data":"x","extra":true}"#);
        assert_eq!(
            parse_publish(&[good.clone(), bad.clone()]),
            Err(SseDirectiveError::Invalid)
        );
        assert_eq!(parse_publish(&[bad, good]), Err(SseDirectiveError::Invalid));
    }

    #[test]
    fn publish_header_name_is_compared_without_case() {
        let events = parse_publish(&[header(
            "X-EDGER-SSE-PUBLISH",
            &encode(r#"{"topic":"t","data":"x"}"#),
        )])
        .expect("valid");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn errors_are_static_and_never_echo_the_guest_value() {
        let secret = "super-secret-token";
        let error = parse_subscribe(&[header(SSE_SUBSCRIBE_HEADER, &format!("{secret} bad"))])
            .expect_err("invalid");
        assert!(!format!("{error} {error:?}").contains(secret));
        let error = parse_publish(&[publish_header(&format!(
            r#"{{"topic":"{secret} x","data":"y"}}"#
        ))])
        .expect_err("invalid");
        assert!(!format!("{error} {error:?}").contains(secret));
    }

    #[test]
    fn strip_removes_only_the_sse_family_in_any_case() {
        let mut headers = vec![
            header("Content-Type", "text/plain"),
            header("x-edger-sse-subscribe", "a"),
            header("X-Edger-Sse-Publish", "b"),
            header("X-EDGER-SSE-Whatever", "c"),
            header("x-edger-ss", "keep"),
            header("x-edger-sse", "keep"),
            header("x-edger-other", "keep"),
            header("set-cookie", "a=1"),
            header("set-cookie", "b=2"),
        ];
        strip_sse_headers(&mut headers);
        let names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Content-Type",
                "x-edger-ss",
                "x-edger-sse",
                "x-edger-other",
                "set-cookie",
                "set-cookie"
            ]
        );
    }
}
