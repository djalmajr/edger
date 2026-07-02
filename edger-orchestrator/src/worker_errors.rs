//! Per-worker recent error ring buffer (story 14.05 — post-deploy transparency).
//!
//! Worker dispatch failures are recorded here so the operator sees *why* a
//! freshly deployed app failed without SSH/log-diving. Root/authenticated
//! reads only; capped per worker to stay bounded.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

const MAX_PER_WORKER: usize = 20;
const MAX_MESSAGE_LEN: usize = 500;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerErrorEntry {
    pub request_id: String,
    pub status: u16,
    pub code: String,
    pub message: String,
    pub at_ms: u128,
}

/// Cloneable handle over a shared per-worker error ring buffer.
#[derive(Clone, Default)]
pub struct WorkerErrorLog {
    inner: Arc<Mutex<HashMap<String, VecDeque<WorkerErrorEntry>>>>,
}

impl WorkerErrorLog {
    pub fn record(&self, worker: &str, request_id: &str, status: u16, code: &str, message: &str) {
        let mut trimmed = strip_ansi(message.trim());
        if trimmed.len() > MAX_MESSAGE_LEN {
            trimmed.truncate(MAX_MESSAGE_LEN);
            trimmed.push('…');
        }
        let entry = WorkerErrorEntry {
            request_id: request_id.to_string(),
            status,
            code: code.to_string(),
            message: trimmed,
            at_ms: now_ms(),
        };
        let Ok(mut map) = self.inner.lock() else {
            return;
        };
        let bucket = map.entry(worker.to_string()).or_default();
        bucket.push_front(entry);
        bucket.truncate(MAX_PER_WORKER);
    }

    /// Most recent errors for a worker, newest first, capped at `limit`.
    pub fn recent(&self, worker: &str, limit: usize) -> Vec<WorkerErrorEntry> {
        let Ok(map) = self.inner.lock() else {
            return Vec::new();
        };
        map.get(worker)
            .map(|bucket| bucket.iter().take(limit).cloned().collect())
            .unwrap_or_default()
    }

    /// Per-worker recent error count + latest entry, for the Workers listing.
    pub fn summary(&self) -> HashMap<String, WorkerErrorSummary> {
        let Ok(map) = self.inner.lock() else {
            return HashMap::new();
        };
        map.iter()
            .filter(|(_, bucket)| !bucket.is_empty())
            .map(|(name, bucket)| {
                (
                    name.clone(),
                    WorkerErrorSummary {
                        count: bucket.len(),
                        latest: bucket.front().cloned(),
                    },
                )
            })
            .collect()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerErrorSummary {
    pub count: usize,
    pub latest: Option<WorkerErrorEntry>,
}

/// Drop ANSI CSI escape sequences (e.g. the color codes Deno writes to
/// stderr) so stored messages read cleanly in the API and UI.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for seq in chars.by_ref() {
                    if seq.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
