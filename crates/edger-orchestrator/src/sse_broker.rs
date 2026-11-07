//! Host-held SSE broker (EDG-55 S1).
//!
//! The guest authorizes and publishes in short executions; this broker keeps
//! the browser connections and fans events out to them.
//!
//! Design:
//! - One bounded queue per subscriber, fed without blocking so a publisher
//!   never waits on a slow client. The queue is a `VecDeque` owned by a small
//!   shared cell that the broker can clear on its own: a retired subscriber
//!   must not keep its frames alive until somebody polls or drops its stream.
//!   There is NO task per connection: the response is a `Stream` that holds
//!   the cell and a guard that returns the slot on `Drop`.
//! - ONE tick task per broker drives the heartbeat and the TTL of every
//!   subscriber. They cannot live in `poll`: hyper does not poll the body of
//!   a client that stopped reading, so a timer there would never fire for
//!   exactly that client. A stopped client fills its queue with heartbeats
//!   and is dropped as `lagged` without anyone polling it.
//! - Each queue holds at most [`SSE_SUBSCRIBER_QUEUE_EVENTS`] frames and
//!   [`SSE_SUBSCRIBER_QUEUE_BYTES`] bytes. Exceeding either removes the
//!   subscriber from its topics, discards the queue on the spot and ends the
//!   stream.
//! - A slot is held from `subscribe` to the `Drop` of the
//!   [`SseSubscription`], and the broker keeps every live subscriber
//!   reachable until then, including one retired by TTL whose queue is still
//!   being delivered: `shutdown` ends those too.
//! - All state sits behind one short `std::sync::Mutex` that is never held
//!   across an `await`; the order per topic is the order of the calls in the
//!   process. Lock order: the broker lock, then a queue lock (a stream only
//!   ever takes its own queue lock). Guarantees: at most once, no replay, no
//!   durability, no global order, one process only.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::convert::Infallible;
use std::hash::{BuildHasher, Hasher};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bytes::Bytes;
use futures_util::Stream;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};

/// Maximum frames queued per subscriber.
pub const SSE_SUBSCRIBER_QUEUE_EVENTS: usize = 64;
/// Maximum bytes queued per subscriber.
pub const SSE_SUBSCRIBER_QUEUE_BYTES: usize = 256 * 1024;

const DEFAULT_MAX_CONNECTIONS: u64 = 1024;
const DEFAULT_MAX_CONNECTIONS_PER_APP: u64 = 256;
const DEFAULT_HEARTBEAT_MS: u64 = 15_000;
const MIN_HEARTBEAT_MS: u64 = 1_000;
/// `retry:` jitter handed to a subscriber whose TTL expired (ms, inclusive).
const RETRY_MIN_MS: u64 = 1_000;
const RETRY_MAX_MS: u64 = 5_000;
/// Fallback horizon when `now + ttl` is not representable.
const FAR_FUTURE: Duration = Duration::from_secs(365 * 24 * 3600);

/// Identity a topic lives under: the whole worker name and its RBAC
/// namespace, both from the final `WorkerRef`. The version is NOT part of it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SseScope {
    pub worker: String,
    pub namespace: Option<String>,
}

/// Host ceilings, resolved once at boot.
#[derive(Clone, Debug)]
pub struct SseLimits {
    /// Connections in the whole process; `0` disables the broker.
    pub max_connections: usize,
    /// Ceiling of the manifest `maxConnections` per app.
    pub max_connections_per_app: usize,
    /// Interval of the `: ping` comment and of the TTL check.
    pub heartbeat: Duration,
}

impl SseLimits {
    /// Read `EDGER_SSE_MAX_CONNECTIONS` (default 1024, `0` disables),
    /// `EDGER_SSE_MAX_CONNECTIONS_PER_APP` (default 256, at least 1) and
    /// `EDGER_SSE_HEARTBEAT_MS` (default 15000, at least 1000). An invalid
    /// value logs a warning and falls back to the default of that variable.
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let max_connections = limit_from(
            "EDGER_SSE_MAX_CONNECTIONS",
            lookup("EDGER_SSE_MAX_CONNECTIONS"),
            DEFAULT_MAX_CONNECTIONS,
            0,
        );
        let per_app = limit_from(
            "EDGER_SSE_MAX_CONNECTIONS_PER_APP",
            lookup("EDGER_SSE_MAX_CONNECTIONS_PER_APP"),
            DEFAULT_MAX_CONNECTIONS_PER_APP,
            1,
        );
        let heartbeat_ms = limit_from(
            "EDGER_SSE_HEARTBEAT_MS",
            lookup("EDGER_SSE_HEARTBEAT_MS"),
            DEFAULT_HEARTBEAT_MS,
            MIN_HEARTBEAT_MS,
        );
        Self {
            max_connections: usize::try_from(max_connections).unwrap_or(usize::MAX),
            max_connections_per_app: usize::try_from(per_app).unwrap_or(usize::MAX),
            heartbeat: Duration::from_millis(heartbeat_ms),
        }
    }
}

fn limit_from(name: &str, raw: Option<String>, default: u64, min: u64) -> u64 {
    let Some(raw) = raw else {
        return default;
    };
    match raw.trim().parse::<u64>() {
        Ok(value) if value >= min => value,
        _ => {
            tracing::warn!(variable = name, "invalid SSE limit; using the default");
            default
        }
    }
}

/// One event for a topic. The parser of slice S3 validates it; the broker
/// still never emits CR or LF in `event` or `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub topic: String,
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
}

/// Why a subscription was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseSubscribeError {
    /// The app ceiling or the process ceiling is full.
    Capacity,
    /// The broker is disabled or shut down.
    Unavailable,
}

/// Counters of one worker (all namespaces), monotonic except `connections`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseWorkerStats {
    pub connections: u64,
    pub published: u64,
    pub disconnect_client: u64,
    pub disconnect_ttl: u64,
    pub disconnect_lagged: u64,
    pub disconnect_shutdown: u64,
    pub rejected_capacity: u64,
    pub rejected_unavailable: u64,
}

/// Point-in-time view of the broker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseStats {
    pub workers: BTreeMap<String, SseWorkerStats>,
    /// Topics with at least one subscriber, across scopes.
    pub topics: usize,
    /// Slots in use (subscribe to `Drop`).
    pub total_connections: usize,
}

#[derive(Clone, Copy)]
enum Retire {
    Client,
    Ttl,
    Lagged,
    Shutdown,
}

/// Lifecycle of a subscriber queue.
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueueState {
    Live,
    /// Retired by TTL: no more frames come in, the pending ones are still
    /// delivered, then the stream ends.
    Draining,
    /// Retired by lag or shutdown, or the stream was dropped: the queue is
    /// empty and the stream ends at its next poll.
    Closed,
}

struct QueueInner {
    bytes: usize,
    frames: VecDeque<Bytes>,
    state: QueueState,
    waker: Option<Waker>,
}

/// The queue of one subscriber, shared by the broker (which fills and
/// clears it) and the stream (which drains it).
struct Queue {
    inner: Mutex<QueueInner>,
}

impl Queue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(QueueInner {
                bytes: 0,
                frames: VecDeque::new(),
                state: QueueState::Live,
                waker: None,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, QueueInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue one frame without blocking; `false` when the event or byte
    /// limit would be exceeded (or the queue no longer accepts frames).
    fn try_enqueue(&self, frame: &Bytes) -> bool {
        let waker = {
            let mut inner = self.lock();
            if inner.state != QueueState::Live
                || inner.frames.len() >= SSE_SUBSCRIBER_QUEUE_EVENTS
                || inner.bytes + frame.len() > SSE_SUBSCRIBER_QUEUE_BYTES
            {
                return false;
            }
            inner.bytes += frame.len();
            inner.frames.push_back(frame.clone());
            inner.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        true
    }

    /// End the stream after the pending frames (TTL).
    fn drain_then_end(&self) {
        let waker = {
            let mut inner = self.lock();
            if inner.state == QueueState::Live {
                inner.state = QueueState::Draining;
            }
            inner.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// End the stream now and release every queued frame (lag, shutdown,
    /// drop).
    fn close_now(&self) {
        let (frames, waker) = {
            let mut inner = self.lock();
            inner.state = QueueState::Closed;
            inner.bytes = 0;
            (std::mem::take(&mut inner.frames), inner.waker.take())
        };
        // The frames are released outside the queue lock.
        drop(frames);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

struct Subscriber {
    expires: Instant,
    queue: Arc<Queue>,
    /// Removed from its topics (and counted as a disconnect). The broker
    /// still holds the subscriber until the stream is dropped.
    retired: bool,
    topics: Vec<String>,
}

#[derive(Default)]
struct ScopeState {
    /// Slots held: every subscriber in `subs` (they leave at their `Drop`).
    connections: usize,
    subs: HashMap<u64, Subscriber>,
    topics: HashMap<String, Vec<u64>>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    scopes: HashMap<SseScope, ScopeState>,
    shutdown: bool,
    stats: BTreeMap<String, SseWorkerStats>,
    total: usize,
}

impl Inner {
    fn bump(&mut self, worker: &str, update: impl FnOnce(&mut SseWorkerStats)) {
        if let Some(stats) = self.stats.get_mut(worker) {
            update(stats);
        } else {
            let mut stats = SseWorkerStats::default();
            update(&mut stats);
            self.stats.insert(worker.to_owned(), stats);
        }
    }

    /// Take a live subscriber out of its topics and end its stream. The
    /// subscriber itself stays reachable until its `Drop`. `false` when it
    /// was already retired.
    fn retire(&mut self, scope: &SseScope, id: u64, reason: Retire) -> bool {
        let Some(state) = self.scopes.get_mut(scope) else {
            return false;
        };
        let Some(sub) = state.subs.get_mut(&id) else {
            return false;
        };
        if sub.retired {
            return false;
        }
        sub.retired = true;
        match reason {
            Retire::Ttl => sub.queue.drain_then_end(),
            Retire::Client | Retire::Lagged | Retire::Shutdown => sub.queue.close_now(),
        }
        let sub_topics = std::mem::take(&mut sub.topics);
        for topic in &sub_topics {
            if let Some(ids) = state.topics.get_mut(topic) {
                ids.retain(|existing| *existing != id);
                if ids.is_empty() {
                    state.topics.remove(topic);
                }
            }
        }
        match reason {
            Retire::Client => self.bump(&scope.worker, |s| s.disconnect_client += 1),
            Retire::Ttl => self.bump(&scope.worker, |s| s.disconnect_ttl += 1),
            Retire::Lagged => {
                self.bump(&scope.worker, |s| s.disconnect_lagged += 1);
                tracing::info!(worker = ?scope.worker, "sse subscriber lagged");
            }
            Retire::Shutdown => self.bump(&scope.worker, |s| s.disconnect_shutdown += 1),
        }
        true
    }

    /// Heartbeat and TTL of every live subscriber (the single broker tick).
    fn tick(&mut self, now: Instant) {
        let ping = Bytes::from_static(b": ping\n\n");
        let mut retired: Vec<(SseScope, u64, Retire)> = Vec::new();
        for (scope, state) in &self.scopes {
            for (id, sub) in state.subs.iter().filter(|(_, sub)| !sub.retired) {
                if now >= sub.expires {
                    let retry = Bytes::from(format!("retry: {}\n\n", retry_jitter_ms()));
                    let reason = if sub.queue.try_enqueue(&retry) {
                        Retire::Ttl
                    } else {
                        Retire::Lagged
                    };
                    retired.push((scope.clone(), *id, reason));
                } else if !sub.queue.try_enqueue(&ping) {
                    retired.push((scope.clone(), *id, Retire::Lagged));
                }
            }
        }
        for (scope, id, reason) in retired {
            self.retire(&scope, id, reason);
        }
    }

    /// End every stream: live subscribers are retired as `shutdown`, and a
    /// stream already retired by TTL loses its pending queue without a
    /// second disconnect count.
    fn shut_down(&mut self) {
        self.shutdown = true;
        let mut live: Vec<(SseScope, u64)> = Vec::new();
        for (scope, state) in &self.scopes {
            for (id, sub) in &state.subs {
                if sub.retired {
                    sub.queue.close_now();
                } else {
                    live.push((scope.clone(), *id));
                }
            }
        }
        for (scope, id) in live {
            self.retire(&scope, id, Retire::Shutdown);
        }
    }
}

struct State {
    inner: Mutex<Inner>,
    limits: SseLimits,
}

impl State {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Per-process SSE broker. Create it inside a Tokio runtime.
pub struct SseBroker {
    state: Arc<State>,
    tick: Mutex<Option<JoinHandle<()>>>,
}

impl SseBroker {
    /// Build the broker and start its single tick task. With
    /// `max_connections == 0` no task is created and every `subscribe`
    /// answers `Unavailable`. Needs a Tokio runtime.
    pub fn new(limits: SseLimits) -> Arc<Self> {
        let period = limits.heartbeat.max(Duration::from_millis(1));
        let enabled = limits.max_connections > 0;
        let state = Arc::new(State {
            inner: Mutex::new(Inner::default()),
            limits,
        });
        let tick = enabled.then(|| {
            let task_state = Arc::clone(&state);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval_at(Instant::now() + period, period);
                interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    let mut inner = task_state.lock();
                    if inner.shutdown {
                        break;
                    }
                    inner.tick(Instant::now());
                }
            })
        });
        Arc::new(Self {
            state,
            tick: Mutex::new(tick),
        })
    }

    /// Open a subscription to `topics` (duplicates collapse). The slot is
    /// taken now and returned when the returned stream is dropped.
    pub fn subscribe(
        &self,
        scope: SseScope,
        mut topics: Vec<String>,
        app_max_connections: usize,
        ttl: Duration,
    ) -> Result<SseSubscription, SseSubscribeError> {
        topics.sort();
        topics.dedup();
        let mut inner = self.state.lock();
        if inner.shutdown || self.state.limits.max_connections == 0 {
            inner.bump(&scope.worker, |s| s.rejected_unavailable += 1);
            return Err(SseSubscribeError::Unavailable);
        }
        let app_ceiling = app_max_connections.min(self.state.limits.max_connections_per_app);
        let app_connections = inner.scopes.get(&scope).map_or(0, |s| s.connections);
        if app_connections >= app_ceiling || inner.total >= self.state.limits.max_connections {
            inner.bump(&scope.worker, |s| s.rejected_capacity += 1);
            return Err(SseSubscribeError::Capacity);
        }

        let id = inner.next_id;
        inner.next_id += 1;
        let queue = Queue::new();
        let now = Instant::now();
        let expires = now.checked_add(ttl).unwrap_or(now + FAR_FUTURE);
        inner.total += 1;
        let state = inner.scopes.entry(scope.clone()).or_default();
        state.connections += 1;
        for topic in &topics {
            state.topics.entry(topic.clone()).or_default().push(id);
        }
        state.subs.insert(
            id,
            Subscriber {
                expires,
                queue: Arc::clone(&queue),
                retired: false,
                topics,
            },
        );
        inner.bump(&scope.worker, |s| s.connections += 1);
        drop(inner);

        Ok(SseSubscription {
            id,
            queue,
            scope,
            state: Arc::clone(&self.state),
        })
    }

    /// Deliver each event to the subscribers of its topic in `scope`. Never
    /// awaits and never blocks on a slow client: a subscriber whose queue
    /// would overflow is removed as `lagged` and its queue is released at
    /// once. An event with no subscriber is dropped without error.
    pub fn publish(&self, scope: &SseScope, events: Vec<SseEvent>) {
        let mut inner = self.state.lock();
        if inner.shutdown {
            return;
        }
        for event in events {
            if has_line_break(event.event.as_deref()) || has_line_break(event.id.as_deref()) {
                tracing::warn!(
                    worker = ?scope.worker,
                    "sse event with CR or LF in event or id dropped"
                );
                continue;
            }
            inner.bump(&scope.worker, |s| s.published += 1);
            let mut lagged: Vec<u64> = Vec::new();
            if let Some(state) = inner.scopes.get(scope) {
                if let Some(ids) = state.topics.get(&event.topic) {
                    let frame = encode_frame(&event);
                    if frame.len() > SSE_SUBSCRIBER_QUEUE_BYTES {
                        tracing::warn!(
                            worker = ?scope.worker,
                            "sse event larger than the subscriber queue dropped"
                        );
                        continue;
                    }
                    for id in ids {
                        if let Some(sub) = state.subs.get(id) {
                            if !sub.queue.try_enqueue(&frame) {
                                lagged.push(*id);
                            }
                        }
                    }
                }
            }
            for id in lagged {
                inner.retire(scope, id, Retire::Lagged);
            }
        }
    }

    /// End every stream (those retired by TTL with a pending queue
    /// included), refuse new subscribers and stop the tick task. Idempotent.
    pub fn shutdown(&self) {
        self.state.lock().shut_down();
        let handle = self.tick.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(handle) = handle {
            handle.abort();
        }
    }

    /// Whether the broker can still admit subscribers: not disabled by
    /// `max_connections == 0` and not shut down. A publish route discards
    /// its directives when this is `false`.
    pub fn is_enabled(&self) -> bool {
        self.state.limits.max_connections > 0 && !self.state.lock().shutdown
    }

    pub fn snapshot(&self) -> SseStats {
        let inner = self.state.lock();
        SseStats {
            topics: inner.scopes.values().map(|state| state.topics.len()).sum(),
            total_connections: inner.total,
            workers: inner.stats.clone(),
        }
    }
}

impl Drop for SseBroker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The response body of one subscriber. Dropping it returns the slot.
pub struct SseSubscription {
    id: u64,
    queue: Arc<Queue>,
    scope: SseScope,
    state: Arc<State>,
}

impl Stream for SseSubscription {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut inner = self.queue.lock();
        match inner.state {
            QueueState::Closed => Poll::Ready(None),
            QueueState::Live | QueueState::Draining => {
                if let Some(frame) = inner.frames.pop_front() {
                    inner.bytes = inner.bytes.saturating_sub(frame.len());
                    Poll::Ready(Some(Ok(frame)))
                } else if inner.state == QueueState::Draining {
                    Poll::Ready(None)
                } else {
                    inner.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        }
    }
}

impl Drop for SseSubscription {
    fn drop(&mut self) {
        let mut inner = self.state.lock();
        // A subscriber still live here ended because the client went away.
        inner.retire(&self.scope, self.id, Retire::Client);
        self.queue.close_now();
        inner.total = inner.total.saturating_sub(1);
        let empty = inner.scopes.get_mut(&self.scope).is_some_and(|state| {
            state.subs.remove(&self.id);
            state.connections = state.connections.saturating_sub(1);
            state.connections == 0
        });
        if empty {
            inner.scopes.remove(&self.scope);
        }
        inner.bump(&self.scope.worker, |s| {
            s.connections = s.connections.saturating_sub(1)
        });
    }
}

fn has_line_break(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.contains(['\r', '\n']))
}

/// Serialize an event once: `event:`, `id:`, one `data:` line per line of
/// `data` (LF, CRLF or CR), then the blank line.
fn encode_frame(event: &SseEvent) -> Bytes {
    let mut frame = String::with_capacity(event.data.len() + 64);
    if let Some(name) = &event.event {
        frame.push_str("event: ");
        frame.push_str(name);
        frame.push('\n');
    }
    if let Some(id) = &event.id {
        frame.push_str("id: ");
        frame.push_str(id);
        frame.push('\n');
    }
    let mut chars = event.data.chars().peekable();
    let mut line = String::new();
    loop {
        match chars.next() {
            Some('\r') => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                push_data_line(&mut frame, &line);
                line.clear();
            }
            Some('\n') => {
                push_data_line(&mut frame, &line);
                line.clear();
            }
            Some(ch) => line.push(ch),
            None => {
                push_data_line(&mut frame, &line);
                break;
            }
        }
    }
    frame.push('\n');
    Bytes::from(frame)
}

fn push_data_line(frame: &mut String, line: &str) {
    frame.push_str("data: ");
    frame.push_str(line);
    frame.push('\n');
}

/// Random `retry:` value in 1000..=5000 ms. Not security sensitive: it only
/// spreads reconnections, so the per-instance random seed of `RandomState`
/// is enough.
fn retry_jitter_ms() -> u64 {
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(CALLS.fetch_add(1, Ordering::Relaxed));
    RETRY_MIN_MS + hasher.finish() % (RETRY_MAX_MS - RETRY_MIN_MS + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(data: &str) -> SseEvent {
        SseEvent {
            topic: "t".into(),
            event: None,
            id: None,
            data: data.into(),
        }
    }

    #[test]
    fn limits_use_the_contract_defaults_when_unset() {
        let limits = SseLimits::from_lookup(|_| None);
        assert_eq!(limits.max_connections, 1024);
        assert_eq!(limits.max_connections_per_app, 256);
        assert_eq!(limits.heartbeat, Duration::from_millis(15_000));
    }

    #[test]
    fn limits_accept_valid_values_and_zero_disables_the_broker() {
        let limits = SseLimits::from_lookup(|name| match name {
            "EDGER_SSE_MAX_CONNECTIONS" => Some("0".into()),
            "EDGER_SSE_MAX_CONNECTIONS_PER_APP" => Some(" 8 ".into()),
            "EDGER_SSE_HEARTBEAT_MS" => Some("1000".into()),
            _ => None,
        });
        assert_eq!(limits.max_connections, 0);
        assert_eq!(limits.max_connections_per_app, 8);
        assert_eq!(limits.heartbeat, Duration::from_millis(1000));
    }

    #[test]
    fn limits_fall_back_to_the_default_per_invalid_variable() {
        let limits = SseLimits::from_lookup(|name| match name {
            "EDGER_SSE_MAX_CONNECTIONS" => Some("-3".into()),
            "EDGER_SSE_MAX_CONNECTIONS_PER_APP" => Some("0".into()),
            "EDGER_SSE_HEARTBEAT_MS" => Some("999".into()),
            _ => None,
        });
        assert_eq!(limits.max_connections, 1024);
        assert_eq!(limits.max_connections_per_app, 256);
        assert_eq!(limits.heartbeat, Duration::from_millis(15_000));
        let text = SseLimits::from_lookup(|_| Some("abc".into()));
        assert_eq!(text.max_connections, 1024);
        assert_eq!(text.heartbeat, Duration::from_millis(15_000));
    }

    #[test]
    fn frames_split_data_on_lf_crlf_and_cr() {
        assert_eq!(
            &encode_frame(&event("a\r\nb\nc\rd"))[..],
            b"data: a\ndata: b\ndata: c\ndata: d\n\n"
        );
        assert_eq!(&encode_frame(&event("a\n"))[..], b"data: a\ndata: \n\n");
        assert_eq!(&encode_frame(&event(""))[..], b"data: \n\n");
        assert_eq!(
            &encode_frame(&event("a\n\nb"))[..],
            b"data: a\ndata: \ndata: b\n\n"
        );
    }

    #[test]
    fn frames_put_event_and_id_before_data() {
        let frame = encode_frame(&SseEvent {
            topic: "t".into(),
            event: Some("msg".into()),
            id: Some("42".into()),
            data: "hi".into(),
        });
        assert_eq!(&frame[..], b"event: msg\nid: 42\ndata: hi\n\n");
    }

    #[test]
    fn retry_jitter_stays_inside_one_to_five_seconds() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2000 {
            let value = retry_jitter_ms();
            assert!((1000..=5000).contains(&value), "{value}");
            seen.insert(value);
        }
        assert!(seen.len() > 100, "the jitter must actually vary");
    }
}
