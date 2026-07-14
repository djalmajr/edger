//! Per-worker admission rate limiting.
//!
//! A fixed 1-second window keyed by worker name. This is availability
//! protection on the deliberately-open data plane (Epic 17): an abusive caller
//! can otherwise consume a worker's whole queue before backpressure kicks in.
//! It is ops-of-runtime (a req/s ceiling declared in the manifest), not app
//! opinion. State is process-global; keying by worker name is sufficient
//! because a worker is a single logical unit regardless of replica.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-key fixed-window counter.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub window_sec: u64,
    pub count: u32,
}

/// Pure decision: update `win` for the current second and report admission.
/// `rps == 0` means unlimited. Returns true if the request is allowed.
pub fn check_window(win: &mut Window, rps: u32, now_sec: u64) -> bool {
    if rps == 0 {
        return true;
    }
    if win.window_sec != now_sec {
        win.window_sec = now_sec;
        win.count = 0;
    }
    if win.count >= rps {
        return false;
    }
    win.count += 1;
    true
}

fn table() -> &'static Mutex<HashMap<String, Window>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Window>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_sec() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Admit one request for `key` under `rps` using the global fixed-1s-window.
pub fn allow(key: &str, rps: u32) -> bool {
    let now = now_sec();
    let mut map = table().lock().expect("rate-limit table poisoned");
    let win = map.entry(key.to_string()).or_default();
    check_window(win, rps, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_when_rps_zero() {
        let mut w = Window::default();
        for _ in 0..1000 {
            assert!(check_window(&mut w, 0, 5));
        }
    }

    #[test]
    fn allows_up_to_rps_then_denies_in_same_window() {
        let mut w = Window::default();
        assert!(check_window(&mut w, 2, 100));
        assert!(check_window(&mut w, 2, 100));
        assert!(!check_window(&mut w, 2, 100));
        assert!(!check_window(&mut w, 2, 100));
    }

    #[test]
    fn resets_on_new_window() {
        let mut w = Window::default();
        assert!(check_window(&mut w, 1, 100));
        assert!(!check_window(&mut w, 1, 100));
        // Next second resets the counter.
        assert!(check_window(&mut w, 1, 101));
        assert!(!check_window(&mut w, 1, 101));
    }

    #[test]
    fn global_allow_smoke() {
        assert!(allow("rl-smoke-worker", 1));
        assert!(!allow("rl-smoke-worker", 1));
    }
}
