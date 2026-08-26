//! Pure security vocabulary and policy helpers.

use crate::{ApiKeyPrincipal, CoreError};

pub const INTERNAL_REQUEST_HEADER: &str = "x-edger-internal";

/// O catálogo INTEIRO de permissions atribuíveis a uma key. `"*"` não é
/// armazenável de propósito: root já é tudo por construção, e manter keys
/// sempre enumeradas deixa a matemática de subconjunto da anti-escalada
/// trivial (nada de "glob concede glob").
pub const PERMISSION_CATALOG: &[&str] = &[
    "workers:read",
    "workers:install",
    "workers:delete",
    "workers:promote",
    "workers:invoke",
    "observability:read",
    "keys:manage",
];

/// Anti-escalada da criação/edição de keys: quem concede não dá o que não
/// tem. Permissions precisam pertencer ao catálogo E (criador não-root) ao
/// conjunto do criador; namespaces e workers concedidos exigem `"*"` do
/// criador ou pertinência LITERAL na lista dele — sem subsunção de glob
/// (decidir se "p-abc*" contém "p-abc-api*" é convite a bug de segurança).
pub fn validate_key_grant(
    creator: &ApiKeyPrincipal,
    permissions: &[String],
    namespaces: &[String],
    workers: &[String],
) -> Result<(), CoreError> {
    if permissions.is_empty() {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            "at least one permission is required",
        ));
    }
    for permission in permissions {
        if !PERMISSION_CATALOG.contains(&permission.as_str()) {
            return Err(CoreError::new(
                "VALIDATION_ERROR",
                format!("unknown permission: {permission}"),
            ));
        }
        if !principal_has_permission(creator, permission) {
            return Err(CoreError::new(
                "KEY_GRANT_DENIED",
                format!("creator lacks permission being granted: {permission}"),
            ));
        }
    }
    validate_scope_grant(creator, "namespace", namespaces, &creator.namespaces)?;
    validate_scope_grant(creator, "worker", workers, &creator.workers)?;
    Ok(())
}

fn validate_scope_grant(
    creator: &ApiKeyPrincipal,
    kind: &str,
    granted: &[String],
    owned: &[String],
) -> Result<(), CoreError> {
    if granted.is_empty() {
        return Err(CoreError::new(
            "VALIDATION_ERROR",
            format!("at least one {kind} entry is required (use \"*\")"),
        ));
    }
    if creator.is_root || owned.iter().any(|entry| entry == "*") {
        return Ok(());
    }
    for entry in granted {
        if !owned.contains(entry) {
            return Err(CoreError::new(
                "KEY_GRANT_DENIED",
                format!("creator cannot grant {kind} scope: {entry}"),
            ));
        }
    }
    Ok(())
}

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
            workers: vec!["*".into()],
            is_root: false,
            expires_at: None,
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
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
    fn key_grant_requires_subset_of_creator() {
        use crate::principal::root_principal;

        let creator = scoped_principal(vec!["@acme"], vec!["keys:manage", "workers:read"]);

        // Subconjunto literal passa.
        assert!(validate_key_grant(
            &creator,
            &strings(&["workers:read"]),
            &strings(&["@acme"]),
            &strings(&["*"]), // criador tem workers ["*"]
        )
        .is_ok());

        // Permission que o criador não tem é negada.
        let err = validate_key_grant(
            &creator,
            &strings(&["workers:install"]),
            &strings(&["@acme"]),
            &strings(&["*"]),
        )
        .unwrap_err();
        assert_eq!(err.code, "KEY_GRANT_DENIED");

        // Namespace fora da lista do criador é negado.
        let err = validate_key_grant(
            &creator,
            &strings(&["workers:read"]),
            &strings(&["@other"]),
            &strings(&["*"]),
        )
        .unwrap_err();
        assert_eq!(err.code, "KEY_GRANT_DENIED");

        // "*" não é permission armazenável nem para root.
        let err = validate_key_grant(
            &root_principal(),
            &strings(&["*"]),
            &strings(&["*"]),
            &strings(&["*"]),
        )
        .unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");

        // Root concede qualquer entrada do catálogo em qualquer escopo.
        assert!(validate_key_grant(
            &root_principal(),
            &strings(&["workers:install", "keys:manage"]),
            &strings(&["@qualquer"]),
            &strings(&["p-abc*"]),
        )
        .is_ok());

        // Listas vazias são inválidas.
        let err =
            validate_key_grant(&creator, &[], &strings(&["*"]), &strings(&["*"])).unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
    }

    #[test]
    fn key_grant_worker_scope_is_literal_membership() {
        let mut creator = scoped_principal(vec!["*"], vec!["keys:manage", "workers:invoke"]);
        creator.workers = strings(&["p-abc*", "hello"]);

        // Entrada literal da lista do criador passa (mesmo sendo glob).
        assert!(validate_key_grant(
            &creator,
            &strings(&["workers:invoke"]),
            &strings(&["*"]),
            &strings(&["hello"]),
        )
        .is_ok());
        assert!(validate_key_grant(
            &creator,
            &strings(&["workers:invoke"]),
            &strings(&["*"]),
            &strings(&["p-abc*"]),
        )
        .is_ok());

        // SEM subsunção de glob: "p-abc-api" não está literalmente na lista.
        let err = validate_key_grant(
            &creator,
            &strings(&["workers:invoke"]),
            &strings(&["*"]),
            &strings(&["p-abc-api"]),
        )
        .unwrap_err();
        assert_eq!(err.code, "KEY_GRANT_DENIED");
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
