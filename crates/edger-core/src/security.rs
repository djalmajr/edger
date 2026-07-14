//! Pure security vocabulary and policy helpers.

use crate::{ApiKeyPrincipal, CoreError};

pub const INTERNAL_REQUEST_HEADER: &str = "x-edger-internal";

pub fn is_mutating_method(method: &str) -> bool {
    matches!(
        method.to_ascii_uppercase().as_str(),
        "POST" | "PUT" | "PATCH" | "DELETE"
    )
}

pub fn principal_has_permission(principal: &ApiKeyPrincipal, permission: &str) -> bool {
    principal.is_root
        || principal
            .permissions
            .iter()
            .any(|candidate| candidate == "*" || candidate == permission)
}

pub fn principal_can_access_optional_namespace(
    principal: &ApiKeyPrincipal,
    namespace: Option<&str>,
) -> bool {
    if principal.is_root {
        return true;
    }
    match namespace {
        Some(namespace) if !namespace.is_empty() => principal
            .namespaces
            .iter()
            .any(|candidate| candidate == "*" || candidate == namespace),
        _ => principal
            .namespaces
            .iter()
            .any(|candidate| candidate == "*"),
    }
}

pub fn require_same_origin(origin: Option<&str>, host: Option<&str>) -> Result<(), CoreError> {
    let origin = origin.ok_or_else(|| CoreError::new("CSRF_DENIED", "origin required"))?;
    let host = host.ok_or_else(|| CoreError::new("CSRF_DENIED", "host required"))?;
    let origin_host = origin_authority(origin)?;
    if origin_host.eq_ignore_ascii_case(host) {
        Ok(())
    } else {
        Err(CoreError::new("CSRF_DENIED", "origin does not match host"))
    }
}

pub fn is_sensitive_env_key(key: &str) -> bool {
    let normalized = key.to_ascii_uppercase();
    normalized.starts_with("AWS_")
        || normalized.starts_with("GITHUB_")
        || normalized.starts_with("OPENAI_")
        || normalized.starts_with("ANTHROPIC_")
        || normalized.starts_with("STRIPE_")
        || normalized.starts_with("DATABASE_")
        || normalized.starts_with("DB_")
        || normalized.starts_with("API_KEY")
        || normalized.starts_with("AUTH_KEY")
        || normalized.starts_with("SECRET_KEY")
        || normalized.starts_with("PRIVATE_KEY")
        || normalized.ends_with("_KEY")
        || normalized.ends_with("_TOKEN")
        || normalized.ends_with("_SECRET")
        || normalized.ends_with("_PASSWORD")
}

fn origin_authority(origin: &str) -> Result<&str, CoreError> {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .ok_or_else(|| CoreError::new("CSRF_DENIED", "origin protocol is not allowed"))?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(CoreError::new(
            "CSRF_DENIED",
            "origin authority is not allowed",
        ));
    }
    Ok(authority)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped_principal(namespaces: Vec<&str>, permissions: Vec<&str>) -> ApiKeyPrincipal {
        ApiKeyPrincipal {
            id: 1,
            name: "operator".into(),
            key_prefix: "operator".into(),
            role: "operator".into(),
            permissions: permissions.into_iter().map(str::to_string).collect(),
            namespaces: namespaces.into_iter().map(str::to_string).collect(),
            is_root: false,
            expires_at: None,
        }
    }

    #[test]
    fn same_origin_requires_http_authority_matching_host() {
        assert!(require_same_origin(Some("https://edger.local"), Some("edger.local")).is_ok());
        assert!(require_same_origin(Some("ftp://edger.local"), Some("edger.local")).is_err());
        assert!(
            require_same_origin(Some("https://user:pass@edger.local"), Some("edger.local"))
                .is_err()
        );
        assert!(require_same_origin(Some("https://evil.local"), Some("edger.local")).is_err());
    }

    #[test]
    fn namespace_access_requires_star_for_unscoped_resources() {
        let acme = scoped_principal(vec!["@acme"], vec!["workers:read"]);
        assert!(principal_can_access_optional_namespace(
            &acme,
            Some("@acme")
        ));
        assert!(!principal_can_access_optional_namespace(
            &acme,
            Some("@other")
        ));
        assert!(!principal_can_access_optional_namespace(&acme, None));

        let wildcard = scoped_principal(vec!["*"], vec!["workers:read"]);
        assert!(principal_can_access_optional_namespace(&wildcard, None));
    }

    #[test]
    fn sensitive_env_patterns_match_runtime_secrets() {
        for key in [
            "DATABASE_URL",
            "DB_PASSWORD",
            "OPENAI_API_KEY",
            "GITHUB_TOKEN",
            "CLIENT_SECRET",
            "PRIVATE_KEY",
            "STRIPE_SECRET_KEY",
        ] {
            assert!(is_sensitive_env_key(key), "{key} should be filtered");
        }
        assert!(!is_sensitive_env_key("PUBLIC_FLAG"));
    }
}
