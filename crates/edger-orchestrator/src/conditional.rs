//! Pure conditional-request evaluation (RFC 9110 §13.1.2) for static
//! responses — EDG-5 part 1.
//!
//! `should_not_modify` and `build_not_modified` are pure functions over
//! plain strings and the serialized response shape, so the RFC rules are
//! table-tested in isolation. Part 2 calls them from `pipeline` on every
//! buffered worker response, before the response leaves `dispatch_worker`.

use edger_core::SerializedResponse;

/// Fields a 304 keeps from the original response (RFC 9110 §15.4.5:
/// Cache-Control, Content-Location, Date, ETag, Expires, Vary). Everything
/// else — in particular the body-describing `content-length`,
/// `content-type` and `content-encoding` — never travels in a 304.
const NOT_MODIFIED_KEEP: &[&str] = &[
    "cache-control",
    "content-location",
    "date",
    "etag",
    "expires",
    "vary",
];

/// Decide whether a response with the given `status` and `response_headers`
/// (the 200 that would be sent) can be replaced by a 304 for a request
/// with the given method and `If-None-Match` header.
///
/// Rules (RFC 9110 §13.1.2):
/// - only GET/HEAD requests;
/// - only a 200 response that carries an `etag` header;
/// - weak comparison: the `W/` prefix is ignored on both sides;
/// - `If-None-Match` may be a comma-separated list and `*`;
/// - a malformed `If-None-Match` value (or a malformed response `etag`) is
///   ignored, so it never produces a 304.
pub fn should_not_modify(
    method: &str,
    if_none_match: Option<&str>,
    status: u16,
    response_headers: &[(String, String)],
) -> bool {
    if !matches!(method, "GET" | "HEAD") || status != 200 {
        return false;
    }
    let Some(request_value) = if_none_match else {
        return false;
    };
    let Some(response_tag) =
        response_etag(response_headers).and_then(|value| parse_entity_tag(value))
    else {
        return false;
    };
    parse_if_none_match(request_value).is_some_and(|candidates| {
        candidates.iter().any(|candidate| {
            candidate == "*" || parse_entity_tag(candidate).is_some_and(|tag| tag == response_tag)
        })
    })
}

/// Build the 304 from the original response: status 304, no body, and only
/// the revalidation fields (see `NOT_MODIFIED_KEEP`) survive, in their
/// original order and casing.
pub fn build_not_modified(original: &SerializedResponse) -> SerializedResponse {
    let headers = original
        .headers
        .iter()
        .filter(|(name, _)| {
            NOT_MODIFIED_KEEP
                .iter()
                .any(|keep| name.eq_ignore_ascii_case(keep))
        })
        .cloned()
        .collect();
    SerializedResponse {
        status: 304,
        headers,
        body: None,
    }
}

fn response_etag(headers: &[(String, String)]) -> Option<&str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("etag"))
        .map(|(_, value)| trim_ows(value))
}

/// OWS is only SP and HTAB (RFC 9110 §5.6.3): anything wider (NBSP,
/// other Unicode whitespace or controls) would normalize a malformed
/// field into a match.
fn trim_ows(value: &str) -> &str {
    value.trim_matches(|c: char| c == ' ' || c == '\t')
}

/// Parse `If-None-Match` (`"*" / 1#entity-tag`). Returns `None` when the
/// value is malformed — a recipient of a malformed field must ignore it
/// (RFC 9110 §8.8.3), never treat it as a match. A comma is a separator
/// only outside a quoted tag ("`a,b`" is one tag), and empty list
/// elements (extra or trailing commas) are ignored, so a value with no
/// valid element yields no candidates (RFC 9110 §5.6.1.2).
fn parse_if_none_match(value: &str) -> Option<Vec<String>> {
    let mut tags = Vec::new();
    for element in split_list_outside_quotes(value) {
        let element = trim_ows(&element);
        if element.is_empty() {
            continue;
        }
        if element != "*" && parse_entity_tag(element).is_none() {
            return None;
        }
        tags.push(element.to_string());
    }
    Some(tags)
}

/// Split a comma-separated field value into raw elements, treating a
/// comma as a separator only when it appears OUTSIDE a quoted
/// entity-tag: "`a,b`" is one element, "`a,b`, `c`" is two.
fn split_list_outside_quotes(value: &str) -> Vec<String> {
    let mut elements = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    for (index, byte) in value.as_bytes().iter().enumerate() {
        match byte {
            b'"' => in_quotes = !in_quotes,
            b',' if !in_quotes => {
                elements.push(value[start..index].to_string());
                start = index + 1;
            }
            _ => {}
        }
    }
    elements.push(value[start..].to_string());
    elements
}

/// Strip the optional `W/` prefix and the quotes of a valid entity-tag
/// (RFC 9110 §8.8), returning the opaque tag (possibly empty). `None` for
/// anything else: `W/` is case-sensitive, the tag must be double-quoted,
/// and every byte of the opaque tag must be an `etagc` character
/// (`%x21 / %x23-7E`) or `obs-text` (bytes `%x80-FF`, surfaced through the
/// `&str` interface as non-ASCII characters). Controls, DEL (`%x7F`),
/// space and internal quotes are rejected.
fn parse_entity_tag(value: &str) -> Option<&str> {
    let tag = value.strip_prefix("W/").unwrap_or(value);
    let inner = tag.strip_prefix('"')?.strip_suffix('"')?;
    if inner
        .bytes()
        .any(|byte| byte <= 0x20 || byte == b'"' || byte == 0x7f)
    {
        return None;
    }
    Some(inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    type HeaderPair = (String, String);

    fn headers(pairs: &[(&str, &str)]) -> Vec<HeaderPair> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn should_not_modify_applies_the_rfc_13_1_2_rules() {
        // (method, If-None-Match, status, response headers, expected 304)
        let etag_strong = headers(&[("etag", r#""abc123""#)]);
        let etag_weak = headers(&[("etag", r#"W/"abc123""#)]);
        let etag_comma = headers(&[("etag", r#"W/"a,b""#)]);
        let etag_empty = headers(&[("etag", r#"W/"""#)]);
        let etag_accent = headers(&[("etag", "\"abcé\"")]);
        let etag_mixed_case_name = headers(&[("ETag", r#""abc123""#)]);
        let etag_malformed = headers(&[("etag", "abc123")]);
        let no_etag = headers(&[("content-type", "text/html; charset=utf-8")]);

        let cases: [(&str, Option<&str>, u16, &Vec<HeaderPair>, bool); 30] = [
            // exact match
            ("GET", Some(r#""abc123""#), 200, &etag_strong, true),
            ("HEAD", Some(r#""abc123""#), 200, &etag_strong, true),
            // weak match: W/ only on the response side
            ("GET", Some(r#""abc123""#), 200, &etag_weak, true),
            // weak match: W/ only on the request side
            ("GET", Some(r#"W/"abc123""#), 200, &etag_strong, true),
            // weak match: W/ on both sides
            ("GET", Some(r#"W/"abc123""#), 200, &etag_weak, true),
            // list (W/ on one item, OWS after the comma)
            ("GET", Some(r#"W/"other", "abc123""#), 200, &etag_weak, true),
            // star
            ("GET", Some("*"), 200, &etag_strong, true),
            // response header name is case-insensitive
            ("GET", Some(r#""abc123""#), 200, &etag_mixed_case_name, true),
            // a comma inside a tag is tag content, not a separator
            ("GET", Some(r#""a,b""#), 200, &etag_comma, true),
            // a comma inside a tag does not split the list
            ("GET", Some(r#""a,b", "abc123""#), 200, &etag_weak, true),
            // trailing comma: the empty element is ignored, not malformed
            ("GET", Some(r#""abc123","#), 200, &etag_strong, true),
            // leading comma: the empty element is ignored
            ("GET", Some(r#", "abc123""#), 200, &etag_strong, true),
            // an empty tag matches an empty tag
            ("GET", Some(r#""""#), 200, &etag_empty, true),
            // obs-text tag matches the same tag
            ("GET", Some("\"abcé\""), 200, &etag_accent, true),
            // SP and HTAB at the edges are OWS and trimmed
            ("GET", Some(" \"abc123\"\t"), 200, &etag_strong, true),
            // no match
            ("GET", Some(r#""deadbeef""#), 200, &etag_strong, false),
            // no If-None-Match
            ("GET", None, 200, &etag_strong, false),
            // POST
            ("POST", Some(r#""abc123""#), 200, &etag_strong, false),
            // status 404
            ("GET", Some(r#""abc123""#), 404, &etag_strong, false),
            // response without etag
            ("GET", Some(r#""abc123""#), 200, &no_etag, false),
            // malformed response etag (unquoted) is ignored
            ("GET", Some(r#""abc123""#), 200, &etag_malformed, false),
            // empty value: no element at all
            ("GET", Some(""), 200, &etag_strong, false),
            // only empty elements: no valid candidate
            ("GET", Some(","), 200, &etag_strong, false),
            // a stray quote after a comma makes the header malformed
            ("GET", Some(r#""abc123",""#), 200, &etag_strong, false),
            // malformed element: unquoted
            ("GET", Some("abc123"), 200, &etag_strong, false),
            // a valid empty tag does not match a non-empty tag
            ("GET", Some(r#""""#), 200, &etag_strong, false),
            // malformed element: W/ without a tag
            ("GET", Some("W/"), 200, &etag_strong, false),
            // valid obs-text tag does not match a different tag
            ("GET", Some("\"abcé\""), 200, &etag_strong, false),
            // NBSP at the edge is not OWS: the element is malformed
            ("GET", Some("\u{00a0}\"abc123\""), 200, &etag_strong, false),
            // a control at the edge is not OWS: the element is malformed
            ("GET", Some("\"abc123\"\u{0001}"), 200, &etag_strong, false),
        ];

        for (method, if_none_match, status, response_headers, expected) in cases {
            assert_eq!(
                should_not_modify(method, if_none_match, status, response_headers),
                expected,
                "case: method={method}, If-None-Match={if_none_match:?}, status={status}"
            );
        }
    }

    #[test]
    fn build_not_modified_keeps_revalidation_fields_and_drops_body_headers() {
        let original = SerializedResponse {
            status: 200,
            headers: headers(&[
                ("content-type", "text/html; charset=utf-8"),
                ("cache-control", "no-cache"),
                ("etag", r#"W/"abc123""#),
                ("vary", "accept-encoding"),
                ("content-location", "/app/index.html"),
                ("date", "Wed, 01 Oct 2026 00:00:00 GMT"),
                ("expires", "Wed, 01 Oct 2026 00:00:01 GMT"),
                ("content-length", "42"),
                ("content-encoding", "gzip"),
            ]),
            body: Some(Bytes::from_static(b"body")),
        };

        let not_modified = build_not_modified(&original);

        assert_eq!(not_modified.status, 304);
        assert!(not_modified.body.is_none());
        assert_eq!(
            not_modified.headers,
            headers(&[
                ("cache-control", "no-cache"),
                ("etag", r#"W/"abc123""#),
                ("vary", "accept-encoding"),
                ("content-location", "/app/index.html"),
                ("date", "Wed, 01 Oct 2026 00:00:00 GMT"),
                ("expires", "Wed, 01 Oct 2026 00:00:01 GMT"),
            ])
        );
        for dropped in ["content-length", "content-type", "content-encoding"] {
            assert!(
                !not_modified
                    .headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(dropped)),
                "{dropped} must not be sent in a 304"
            );
        }
    }

    #[test]
    fn build_not_modified_of_response_without_revalidation_fields_is_empty() {
        let original = SerializedResponse {
            status: 200,
            headers: headers(&[("content-type", "text/plain")]),
            body: Some(Bytes::from_static(b"x")),
        };

        let not_modified = build_not_modified(&original);

        assert_eq!(not_modified.status, 304);
        assert!(not_modified.headers.is_empty());
        assert!(not_modified.body.is_none());
    }
}
