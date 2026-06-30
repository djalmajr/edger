//! HTTP security guards for operational routes.

use axum::http::HeaderMap;
use edger_core::{
    is_mutating_method, require_same_origin, ApiKeyPrincipal, CoreError, INTERNAL_REQUEST_HEADER,
};

pub fn validate_admin_mutation_security(
    method: &str,
    headers: &HeaderMap,
    principal: &ApiKeyPrincipal,
) -> Result<(), CoreError> {
    if !is_mutating_method(method) {
        return Ok(());
    }

    if header_is_true(headers, INTERNAL_REQUEST_HEADER) {
        if principal.is_root {
            return Ok(());
        }
        return Err(CoreError::new(
            "FORBIDDEN",
            "internal requests require root credentials",
        ));
    }

    if is_browser_originated(headers) {
        return require_same_origin(
            header_value(headers, "origin"),
            header_value(headers, "host"),
        );
    }

    Ok(())
}

fn is_browser_originated(headers: &HeaderMap) -> bool {
    header_value(headers, "origin").is_some()
        || header_value(headers, "sec-fetch-mode").is_some()
        || header_value(headers, "sec-fetch-site").is_some()
}

fn header_is_true(headers: &HeaderMap, name: &str) -> bool {
    header_value(headers, name).is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}
