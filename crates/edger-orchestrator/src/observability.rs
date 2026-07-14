//! Bounded, queryable operational events for the local cPanel.
//!
//! This store is intentionally in-memory and allowlisted. It is the local
//! source of truth; external exporters consume the same envelope later.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::broadcast;

const MAX_MESSAGE_LEN: usize = 500;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalEventSource {
    Runtime,
    Orchestrator,
    Release,
    Drain,
    Console,
}

impl OperationalEventSource {
    fn label(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Orchestrator => "orchestrator",
            Self::Release => "release",
            Self::Drain => "drain",
            Self::Console => "console",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalEventLevel {
    Info,
    Warn,
    Error,
}

impl OperationalEventLevel {
    fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug)]
pub struct OperationalEventInput {
    pub source: OperationalEventSource,
    pub kind: String,
    pub level: OperationalEventLevel,
    pub namespace: Option<String>,
    pub worker: Option<String>,
    pub version: Option<String>,
    pub process_id: Option<String>,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub outcome: Option<String>,
    pub status: Option<u16>,
    pub duration_ms: Option<u64>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub truncated: Option<bool>,
    pub dropped_count: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationalEvent {
    pub id: u64,
    pub at_ms: u128,
    pub source: OperationalEventSource,
    pub kind: String,
    pub level: OperationalEventLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_count: Option<u64>,
}

impl OperationalEvent {
    fn identity_matches(&self, other: &Self) -> bool {
        self.namespace == other.namespace
            && self.worker == other.worker
            && self.version == other.version
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OperationalStoreConfig {
    pub global_capacity: usize,
    pub per_identity_capacity: usize,
}

impl Default for OperationalStoreConfig {
    fn default() -> Self {
        Self {
            global_capacity: 2_000,
            per_identity_capacity: 200,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OperationalEventQuery {
    pub before: Option<u64>,
    pub limit: Option<usize>,
    pub since_ms: Option<u128>,
    pub until_ms: Option<u128>,
    pub namespace: Option<String>,
    pub worker: Option<String>,
    pub version: Option<String>,
    pub process_id: Option<String>,
    pub source: Option<String>,
    pub kind: Option<String>,
    pub level: Option<String>,
    pub outcome: Option<String>,
    pub status: Option<u16>,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationalStoreStats {
    pub capacity: usize,
    pub size: usize,
    pub evicted: u64,
    pub dropped: u64,
    pub truncated: u64,
    pub oldest_id: Option<u64>,
    pub newest_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationalEventPage {
    pub events: Vec<OperationalEvent>,
    pub next_cursor: Option<u64>,
    pub stats: OperationalStoreStats,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationalEventTail {
    pub events: Vec<OperationalEvent>,
    pub gap: bool,
    pub oldest_available: Option<u64>,
    pub newest_available: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OperationalSeriesPoint {
    pub start_ms: u128,
    pub request_count: u64,
    pub error_count: u64,
    pub duration_p95_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OperationalSeries {
    pub window_ms: u64,
    pub bucket_ms: u64,
    pub store_started_at_ms: u128,
    pub partial_window: bool,
    pub points: Vec<OperationalSeriesPoint>,
}

struct OperationalStoreInner {
    events: VecDeque<OperationalEvent>,
    next_id: u64,
    evicted: u64,
    dropped: u64,
    truncated: u64,
    started_at_ms: u128,
}

impl Default for OperationalStoreInner {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            next_id: 0,
            evicted: 0,
            dropped: 0,
            truncated: 0,
            started_at_ms: now_ms(),
        }
    }
}

#[derive(Clone)]
pub struct OperationalStore {
    config: OperationalStoreConfig,
    inner: Arc<Mutex<OperationalStoreInner>>,
    notifier: broadcast::Sender<u64>,
}

impl Default for OperationalStore {
    fn default() -> Self {
        Self::new(OperationalStoreConfig::default())
    }
}

impl OperationalStore {
    pub fn new(config: OperationalStoreConfig) -> Self {
        let (notifier, _) = broadcast::channel(256);
        Self {
            config: OperationalStoreConfig {
                global_capacity: config.global_capacity.max(1),
                per_identity_capacity: config.per_identity_capacity.max(1),
            },
            inner: Arc::new(Mutex::new(OperationalStoreInner::default())),
            notifier,
        }
    }

    pub fn record(&self, input: OperationalEventInput) -> Option<u64> {
        let Ok(mut inner) = self.inner.lock() else {
            return None;
        };
        inner.dropped = inner
            .dropped
            .saturating_add(input.dropped_count.unwrap_or_default());
        if input.truncated.unwrap_or(false) {
            inner.truncated = inner.truncated.saturating_add(1);
        }
        inner.next_id = inner.next_id.saturating_add(1);
        let event = OperationalEvent {
            id: inner.next_id,
            at_ms: now_ms(),
            source: input.source,
            kind: sanitize_token(&input.kind),
            level: input.level,
            namespace: input.namespace.map(|value| sanitize_token(&value)),
            worker: input.worker.map(|value| sanitize_token(&value)),
            version: input.version.map(|value| sanitize_token(&value)),
            process_id: input.process_id.map(|value| sanitize_token(&value)),
            request_id: input.request_id.map(|value| sanitize_token(&value)),
            trace_id: input.trace_id.map(|value| sanitize_token(&value)),
            outcome: input.outcome.map(|value| sanitize_token(&value)),
            status: input.status,
            duration_ms: input.duration_ms,
            code: input.code.map(|value| sanitize_token(&value)),
            message: input.message.map(|value| sanitize_message(&value)),
            truncated: input.truncated,
            dropped_count: input.dropped_count,
        };
        let id = event.id;
        inner.events.push_back(event.clone());

        while inner
            .events
            .iter()
            .filter(|candidate| candidate.identity_matches(&event))
            .count()
            > self.config.per_identity_capacity
        {
            if let Some(index) = inner
                .events
                .iter()
                .position(|candidate| candidate.identity_matches(&event))
            {
                inner.events.remove(index);
                inner.evicted = inner.evicted.saturating_add(1);
            } else {
                break;
            }
        }
        while inner.events.len() > self.config.global_capacity {
            inner.events.pop_front();
            inner.evicted = inner.evicted.saturating_add(1);
        }
        drop(inner);
        emit_operational_event(&event);
        let _ = self.notifier.send(id);
        Some(id)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<u64> {
        self.notifier.subscribe()
    }

    pub fn tail(&self, query: OperationalEventQuery, after: u64) -> OperationalEventTail {
        let Ok(inner) = self.inner.lock() else {
            return OperationalEventTail {
                events: Vec::new(),
                gap: true,
                oldest_available: None,
                newest_available: None,
            };
        };
        let oldest_available = inner.events.front().map(|event| event.id);
        let newest_available = inner.events.back().map(|event| event.id);
        let gap =
            after > 0 && oldest_available.is_some_and(|oldest| oldest > after.saturating_add(1));
        let limit = query.limit.unwrap_or(100).clamp(1, 200);
        let events = inner
            .events
            .iter()
            .filter(|event| event.id > after && event_matches(event, &query))
            .take(limit)
            .cloned()
            .collect();
        OperationalEventTail {
            events,
            gap,
            oldest_available,
            newest_available,
        }
    }

    pub fn query(&self, query: OperationalEventQuery) -> OperationalEventPage {
        let Ok(inner) = self.inner.lock() else {
            return OperationalEventPage {
                events: Vec::new(),
                next_cursor: None,
                stats: OperationalStoreStats {
                    capacity: self.config.global_capacity,
                    dropped: 1,
                    ..Default::default()
                },
            };
        };
        let limit = query.limit.unwrap_or(100).clamp(1, 200);
        let mut matches = inner
            .events
            .iter()
            .rev()
            .filter(|event| event_matches(event, &query));
        let events: Vec<_> = matches.by_ref().take(limit).cloned().collect();
        let next_cursor = if matches.next().is_some() {
            events.last().map(|event| event.id)
        } else {
            None
        };
        OperationalEventPage {
            events,
            next_cursor,
            stats: OperationalStoreStats {
                capacity: self.config.global_capacity,
                size: inner.events.len(),
                evicted: inner.evicted,
                dropped: inner.dropped,
                truncated: inner.truncated,
                oldest_id: inner.events.front().map(|event| event.id),
                newest_id: inner.events.back().map(|event| event.id),
            },
        }
    }

    pub fn series(
        &self,
        mut query: OperationalEventQuery,
        window_ms: u64,
        bucket_ms: u64,
    ) -> OperationalSeries {
        let window_ms = window_ms.clamp(30_000, 15 * 60_000);
        let bucket_ms = bucket_ms.clamp(5_000, 60_000).min(window_ms);
        let end_ms = now_ms();
        let start_ms = end_ms.saturating_sub(window_ms as u128);
        query.since_ms = Some(start_ms);
        query.until_ms = Some(end_ms);
        query.kind = Some("dispatch".into());

        let Ok(inner) = self.inner.lock() else {
            return OperationalSeries {
                window_ms,
                bucket_ms,
                store_started_at_ms: end_ms,
                partial_window: true,
                points: Vec::new(),
            };
        };
        let mut buckets: BTreeMap<u128, (u64, u64, Vec<u64>)> = BTreeMap::new();
        let mut cursor = start_ms - (start_ms % bucket_ms as u128);
        while cursor <= end_ms {
            buckets.insert(cursor, (0, 0, Vec::new()));
            cursor = cursor.saturating_add(bucket_ms as u128);
        }
        for event in inner
            .events
            .iter()
            .filter(|event| event_matches(event, &query))
        {
            let bucket = event.at_ms - (event.at_ms % bucket_ms as u128);
            let entry = buckets.entry(bucket).or_default();
            entry.0 = entry.0.saturating_add(1);
            if event.level == OperationalEventLevel::Error
                || event.status.is_some_and(|status| status >= 500)
                || event
                    .outcome
                    .as_deref()
                    .is_some_and(|outcome| outcome != "ok")
            {
                entry.1 = entry.1.saturating_add(1);
            }
            if let Some(duration_ms) = event.duration_ms {
                entry.2.push(duration_ms);
            }
        }
        let points = buckets
            .into_iter()
            .map(|(start_ms, (request_count, error_count, mut durations))| {
                durations.sort_unstable();
                let duration_p95_ms = if durations.is_empty() {
                    None
                } else {
                    let index = (durations.len() * 95).div_ceil(100).saturating_sub(1);
                    Some(durations[index])
                };
                OperationalSeriesPoint {
                    start_ms,
                    request_count,
                    error_count,
                    duration_p95_ms,
                }
            })
            .collect();
        OperationalSeries {
            window_ms,
            bucket_ms,
            store_started_at_ms: inner.started_at_ms,
            partial_window: inner.started_at_ms > start_ms || inner.evicted > 0,
            points,
        }
    }
}

fn emit_operational_event(event: &OperationalEvent) {
    macro_rules! emit {
        ($macro:ident) => {
            tracing::$macro!(
                target: "edger_orchestrator::operational_event",
                event_id = event.id,
                event_source = event.source.label(),
                event_kind = event.kind.as_str(),
                event_level = event.level.label(),
                worker_namespace = event.namespace.as_deref().unwrap_or(""),
                worker_name = event.worker.as_deref().unwrap_or(""),
                worker_version = event.version.as_deref().unwrap_or(""),
                process_id = event.process_id.as_deref().unwrap_or(""),
                request_id = event.request_id.as_deref().unwrap_or(""),
                trace_id = event.trace_id.as_deref().unwrap_or(""),
                event_outcome = event.outcome.as_deref().unwrap_or(""),
                http_status_code = event.status.unwrap_or_default(),
                event_duration_ms = event.duration_ms.unwrap_or_default(),
                error_code = event.code.as_deref().unwrap_or(""),
                event_message = event.message.as_deref().unwrap_or(""),
                event_truncated = event.truncated.unwrap_or(false),
                event_dropped_count = event.dropped_count.unwrap_or_default(),
                "operational event"
            )
        };
    }

    match event.level {
        OperationalEventLevel::Info => emit!(info),
        OperationalEventLevel::Warn => emit!(warn),
        OperationalEventLevel::Error => emit!(error),
    }
}

fn event_matches(event: &OperationalEvent, query: &OperationalEventQuery) -> bool {
    query.before.is_none_or(|before| event.id < before)
        && query.since_ms.is_none_or(|since| event.at_ms >= since)
        && query.until_ms.is_none_or(|until| event.at_ms <= until)
        && option_matches(&query.namespace, &event.namespace)
        && option_matches(&query.worker, &event.worker)
        && option_matches(&query.version, &event.version)
        && option_matches(&query.process_id, &event.process_id)
        && query
            .source
            .as_deref()
            .is_none_or(|source| event.source.label().eq_ignore_ascii_case(source))
        && query
            .kind
            .as_deref()
            .is_none_or(|kind| event.kind.eq_ignore_ascii_case(kind))
        && query
            .level
            .as_deref()
            .is_none_or(|level| event.level.label().eq_ignore_ascii_case(level))
        && option_matches(&query.outcome, &event.outcome)
        && query
            .status
            .is_none_or(|status| event.status == Some(status))
        && option_matches(&query.request_id, &event.request_id)
        && option_matches(&query.trace_id, &event.trace_id)
}

fn option_matches(filter: &Option<String>, value: &Option<String>) -> bool {
    filter
        .as_deref()
        .is_none_or(|expected| value.as_deref() == Some(expected))
}

fn sanitize_token(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .take(128)
        .collect()
}

fn sanitize_message(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if [
        "authorization",
        "cookie",
        "password",
        "secret",
        "token",
        "api_key",
        "api-key",
        "body",
        "file://",
        "/users/",
        "/home/",
        "/var/",
        "/tmp/",
        "\\users\\",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return "[redacted]".into();
    }
    value
        .chars()
        .filter(|ch| !ch.is_control() || *ch == ' ')
        .take(MAX_MESSAGE_LEN)
        .collect()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
