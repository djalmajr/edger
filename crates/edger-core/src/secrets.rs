//! App secret vocabulary (EDG-38): the resolver contract the process
//! backend calls at spawn, and the name rule the Admin API applies.
//!
//! Core stays I/O-free: this module declares the pure signature only.
//! The implementation (SQLite store + ChaCha20-Poly1305 at rest, master
//! key from `EDGER_SECRETS_KEY_FILE`) lives in the orchestrator.

use async_trait::async_trait;

use crate::error::CoreError;

/// Resolves an app secret declared in a worker manifest (`secrets`) to its
/// value, from the EdgeR vault (EDG-38).
///
/// `None` is the fail-closed answer: the vault is unavailable (no master
/// key) or the `(worker, name)` pair has no record that decrypts. The
/// caller logs one warn per name at every process spawn and spawns the
/// worker without the env var — the worker still starts. The value never appears in any
/// log, response or operational event; only the NAME may be logged.
#[async_trait]
pub trait AppSecretResolver: Send + Sync {
    /// The decrypted value for `(worker, name)`, or `None` (fail closed).
    async fn resolve_secret(&self, worker: &str, name: &str) -> Option<String>;
}

/// Shape rule for app secret names (`^[A-Z_][A-Z0-9_]{0,63}$`), shared by
/// the manifest `secrets` validation and the Admin API write path.
pub fn is_valid_secret_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let head = bytes[0];
    if !(head.is_ascii_uppercase() || head == b'_') {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b'_')
}

/// Full name rule for a secret written through the Admin API (EDG-38): the
/// manifest `secrets` shape plus the reserved `EDGER_` prefix the runtime
/// keeps for its own injected keys. The manifest-local collision checks
/// (env keys, `publicEnv`, `DENO_DIR`) belong to the manifest itself.
pub fn validate_app_secret_name(name: &str) -> Result<(), CoreError> {
    if !is_valid_secret_name(name) {
        return Err(CoreError::validation(
            "secret",
            "secret names must match ^[A-Z_][A-Z0-9_]{0,63}$",
        ));
    }
    if name.starts_with("EDGER_") {
        return Err(CoreError::validation(
            "secret",
            "secret names must not start with EDGER_ (runtime-injected keys are reserved)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_rule_accepts_the_manifest_shape() {
        assert!(is_valid_secret_name("BYOK_MASTER_KEY"));
        assert!(is_valid_secret_name("_LEADING"));
        assert!(is_valid_secret_name("A"));
        assert!(is_valid_secret_name(&"A".repeat(64)));
    }

    #[test]
    fn name_rule_rejects_hostile_shapes() {
        assert!(!is_valid_secret_name(""));
        assert!(!is_valid_secret_name("lower"));
        assert!(!is_valid_secret_name("1ABC"));
        assert!(!is_valid_secret_name("WITH-DASH"));
        assert!(!is_valid_secret_name("WITH.DOT"));
        assert!(!is_valid_secret_name("WITH SPACE"));
        assert!(!is_valid_secret_name(&"A".repeat(65)));
        assert!(!is_valid_secret_name("AB;DROP TABLE"));
    }

    #[test]
    fn the_runtime_reserved_prefix_is_refused_by_the_write_rule() {
        assert!(validate_app_secret_name("BYOK_MASTER_KEY").is_ok());
        let err = validate_app_secret_name("EDGER_EGRESS_SOCKET").unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
        assert!(err.message.contains("EDGER_"));
        let err = validate_app_secret_name("lower").unwrap_err();
        assert_eq!(err.code, "VALIDATION_ERROR");
    }
}
