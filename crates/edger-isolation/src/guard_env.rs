//! Env names reserved for the egress guard (EDG-38), shared by every backend
//! that starts a Deno process (the persistent process runtime and the CLI
//! bridge).
//!
//! `EDGER_EGRESS_SOCKET` / `EDGER_EGRESS_HOST` exist in a worker's env ONLY
//! while the guard socket is live for that very process, so a worker can use
//! their presence as the signal that the guard exists. Only the persistent
//! process spawn sets them. Anything coming from the manifest `env:` or from
//! a resolved app secret under these names is dropped, in every backend, so
//! the presence cannot be forged.

/// Env var carrying the guard socket path. It exists only when the socket
/// is live for that process.
pub const EGRESS_SOCKET_ENV: &str = "EDGER_EGRESS_SOCKET";

/// Env var carrying the never-resolving guard host, injected together with
/// the socket.
pub const EGRESS_HOST_ENV: &str = "EDGER_EGRESS_HOST";

/// Whether `key` is one of the names reserved for the guard
/// (case-insensitive, like `DENO_DIR`).
pub fn is_reserved_guard_env(key: &str) -> bool {
    key.eq_ignore_ascii_case(EGRESS_SOCKET_ENV) || key.eq_ignore_ascii_case(EGRESS_HOST_ENV)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_guard_names_are_reserved_in_any_case() {
        for key in [
            "EDGER_EGRESS_SOCKET",
            "edger_egress_socket",
            "Edger_Egress_Host",
            "EDGER_EGRESS_HOST",
        ] {
            assert!(is_reserved_guard_env(key), "{key}");
        }
        for key in [
            "EDGER_EGRESS_SOCKET_X",
            "XEDGER_EGRESS_HOST",
            "EDGER_EGRESS",
            "DENO_DIR",
            "",
        ] {
            assert!(!is_reserved_guard_env(key), "{key:?}");
        }
    }
}
