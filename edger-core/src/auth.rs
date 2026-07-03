//! Auth header helpers.

/// Header map as owned pairs (pure; orchestrator converts from hyper).
pub type HeaderPairs = [(String, String)];

/// Extract API key from header pairs (`Authorization: Bearer` or `x-api-key`).
pub fn extract_api_key_from_pairs(headers: &[(String, String)]) -> Option<String> {
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("authorization") {
            let prefix = "Bearer ";
            if let Some(token) = value.strip_prefix(prefix) {
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("x-api-key") && !value.is_empty() {
            return Some(value.clone());
        }
    }
    None
}
