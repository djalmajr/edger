//! EDG-55 S1: the host-held SSE broker, on a paused clock.
//!
//! The broker has ONE tick task (heartbeat and TTL). With
//! `tokio::time::pause` the tests step it one heartbeat at a time, so every
//! timing assertion is deterministic. The concurrency test is the only one
//! on a multi-thread runtime with the real clock.

use std::time::Duration;

use bytes::Bytes;
use edger_orchestrator::sse_broker::{
    SseBroker, SseEvent, SseLimits, SseScope, SseSubscribeError, SseSubscription,
    SSE_SUBSCRIBER_QUEUE_EVENTS,
};
use futures_util::{FutureExt, StreamExt};

const HOUR: Duration = Duration::from_secs(3600);
const SECOND: Duration = Duration::from_secs(1);
const PING: &[u8] = b": ping\n\n";

/// What one non-blocking read of a stream saw.
#[derive(Debug, PartialEq, Eq)]
enum Read {
    Pending,
    Ended,
    Frame(Bytes),
}

fn scope(worker: &str) -> SseScope {
    SseScope {
        worker: worker.into(),
        namespace: None,
    }
}

fn limits(max: usize, per_app: usize, heartbeat: Duration) -> SseLimits {
    SseLimits {
        max_connections: max,
        max_connections_per_app: per_app,
        heartbeat,
    }
}

fn event(topic: &str, data: &str) -> SseEvent {
    SseEvent {
        topic: topic.into(),
        event: None,
        id: None,
        data: data.into(),
    }
}

fn topics(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

fn read(subscription: &mut SseSubscription) -> Read {
    match subscription.next().now_or_never() {
        None => Read::Pending,
        Some(None) => Read::Ended,
        Some(Some(Ok(frame))) => Read::Frame(frame),
    }
}

/// Every frame ready right now, and whether the stream ended after them.
fn drain(subscription: &mut SseSubscription) -> (Vec<Bytes>, bool) {
    let mut frames = Vec::new();
    loop {
        match read(subscription) {
            Read::Frame(frame) => frames.push(frame),
            Read::Pending => return (frames, false),
            Read::Ended => return (frames, true),
        }
    }
}

fn frames_of(subscription: &mut SseSubscription) -> Vec<Bytes> {
    drain(subscription).0
}

/// Create the broker and let its tick task register its timer before the
/// test moves the paused clock.
async fn broker(limits: SseLimits) -> std::sync::Arc<SseBroker> {
    let broker = SseBroker::new(limits);
    settle().await;
    broker
}

async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

async fn advance(duration: Duration) {
    tokio::time::advance(duration).await;
    settle().await;
}

fn open(
    broker: &SseBroker,
    app: &str,
    names: &[&str],
    app_max: usize,
    ttl: Duration,
) -> SseSubscription {
    broker
        .subscribe(scope(app), topics(names), app_max, ttl)
        .expect("subscription admitted")
}

#[tokio::test(start_paused = true)]
async fn fan_out_reaches_20_subscribers_with_the_exact_frame_and_one_shared_buffer() {
    let broker = broker(limits(1024, 256, HOUR)).await;
    let mut subscribers: Vec<SseSubscription> = (0..20)
        .map(|_| open(&broker, "hub", &["room.1"], 256, HOUR))
        .collect();
    let mut bystander = open(&broker, "hub", &["room.2"], 256, HOUR);

    broker.publish(
        &scope("hub"),
        vec![SseEvent {
            topic: "room.1".into(),
            event: Some("msg".into()),
            id: Some("7".into()),
            data: "hello".into(),
        }],
    );

    let mut pointers = Vec::new();
    for subscriber in &mut subscribers {
        let frames = frames_of(subscriber);
        assert_eq!(frames.len(), 1);
        assert_eq!(&frames[0][..], b"event: msg\nid: 7\ndata: hello\n\n");
        pointers.push(frames[0].as_ptr());
    }
    assert!(
        pointers.windows(2).all(|pair| pair[0] == pair[1]),
        "the event is serialized once and shared"
    );
    assert_eq!(
        read(&mut bystander),
        Read::Pending,
        "other topic gets nothing"
    );
    let stats = broker.snapshot();
    assert_eq!(stats.total_connections, 21);
    assert_eq!(stats.topics, 2);
    assert_eq!(stats.workers["hub"].connections, 21);
    assert_eq!(stats.workers["hub"].published, 1);
}

#[tokio::test(start_paused = true)]
async fn multi_line_data_becomes_one_data_line_per_line() {
    let broker = broker(limits(8, 8, HOUR)).await;
    let mut subscriber = open(&broker, "hub", &["t"], 8, HOUR);
    broker.publish(
        &scope("hub"),
        vec![
            event("t", "a\r\nb\nc\rd"),
            event("t", "single"),
            event("t", "tail\n"),
        ],
    );
    let frames = frames_of(&mut subscriber);
    assert_eq!(frames.len(), 3);
    assert_eq!(&frames[0][..], b"data: a\ndata: b\ndata: c\ndata: d\n\n");
    assert_eq!(&frames[1][..], b"data: single\n\n");
    assert_eq!(&frames[2][..], b"data: tail\ndata: \n\n");
}

#[tokio::test(start_paused = true)]
async fn cr_or_lf_in_event_or_id_is_dropped_and_never_reaches_the_wire() {
    let broker = broker(limits(8, 8, HOUR)).await;
    let mut subscriber = open(&broker, "hub", &["t"], 8, HOUR);
    let bad_event = SseEvent {
        event: Some("a\nevent: injected".into()),
        ..event("t", "x")
    };
    let bad_id = SseEvent {
        id: Some("1\rid: 2".into()),
        ..event("t", "y")
    };
    broker.publish(&scope("hub"), vec![bad_event, bad_id, event("t", "ok")]);
    let frames = frames_of(&mut subscriber);
    assert_eq!(frames.len(), 1, "only the valid event is delivered");
    assert_eq!(&frames[0][..], b"data: ok\n\n");
    assert_eq!(broker.snapshot().workers["hub"].published, 1);
}

#[tokio::test(start_paused = true)]
async fn same_topic_name_is_isolated_per_worker_and_per_namespace() {
    let broker = broker(limits(64, 64, HOUR)).await;
    let scopes = [
        scope("app-a"),
        scope("app-b"),
        SseScope {
            worker: "app-a".into(),
            namespace: Some("@x".into()),
        },
        SseScope {
            worker: "app-a".into(),
            namespace: Some("@y".into()),
        },
    ];
    let mut subscribers: Vec<SseSubscription> = scopes
        .iter()
        .map(|scope| {
            broker
                .subscribe(scope.clone(), topics(&["same"]), 8, HOUR)
                .expect("admitted")
        })
        .collect();

    broker.publish(&scopes[0], vec![event("same", "for-a")]);
    assert_eq!(frames_of(&mut subscribers[0]).len(), 1);
    for other in &mut subscribers[1..] {
        assert_eq!(read(other), Read::Pending);
    }

    broker.publish(&scopes[2], vec![event("same", "for-a-x")]);
    assert_eq!(frames_of(&mut subscribers[2]).len(), 1);
    assert_eq!(read(&mut subscribers[0]), Read::Pending);
    assert_eq!(read(&mut subscribers[3]), Read::Pending);
    assert_eq!(read(&mut subscribers[1]), Read::Pending);
}

#[tokio::test(start_paused = true)]
async fn app_ceiling_is_exact_does_not_touch_existing_subscribers_and_returns_on_drop() {
    let broker = broker(limits(100, 100, HOUR)).await;
    let mut first = open(&broker, "hub", &["t"], 3, HOUR);
    let second = open(&broker, "hub", &["t"], 3, HOUR);
    let third = open(&broker, "hub", &["t"], 3, HOUR);

    let refused = broker.subscribe(scope("hub"), topics(&["t"]), 3, HOUR);
    assert_eq!(refused.err(), Some(SseSubscribeError::Capacity));
    assert_eq!(broker.snapshot().workers["hub"].rejected_capacity, 1);
    assert_eq!(broker.snapshot().total_connections, 3);

    broker.publish(&scope("hub"), vec![event("t", "still-delivered")]);
    assert_eq!(frames_of(&mut first).len(), 1, "Capacity left it intact");

    // Another app is not charged to this one.
    let _other = open(&broker, "other", &["t"], 3, HOUR);

    drop(second);
    let again = broker
        .subscribe(scope("hub"), topics(&["t"]), 3, HOUR)
        .expect("the slot came back on Drop");
    drop((again, third));
    assert_eq!(broker.snapshot().workers["hub"].connections, 1);
}

#[tokio::test(start_paused = true)]
async fn host_per_app_ceiling_caps_the_manifest_value() {
    let broker = broker(limits(100, 2, HOUR)).await;
    let _a = open(&broker, "hub", &["t"], 50, HOUR);
    let _b = open(&broker, "hub", &["t"], 50, HOUR);
    assert_eq!(
        broker
            .subscribe(scope("hub"), topics(&["t"]), 50, HOUR)
            .err(),
        Some(SseSubscribeError::Capacity)
    );
    assert_eq!(
        broker
            .subscribe(scope("hub"), topics(&["t"]), 0, HOUR)
            .err(),
        Some(SseSubscribeError::Capacity),
        "a manifest value of zero admits nobody"
    );
}

#[tokio::test(start_paused = true)]
async fn process_ceiling_is_exact_across_apps_and_returns_on_drop() {
    let broker = broker(limits(4, 100, HOUR)).await;
    let a1 = open(&broker, "app-a", &["t"], 100, HOUR);
    let _a2 = open(&broker, "app-a", &["t"], 100, HOUR);
    let _b1 = open(&broker, "app-b", &["t"], 100, HOUR);
    let _b2 = open(&broker, "app-b", &["t"], 100, HOUR);

    for app in ["app-a", "app-b", "app-c"] {
        assert_eq!(
            broker
                .subscribe(scope(app), topics(&["t"]), 100, HOUR)
                .err(),
            Some(SseSubscribeError::Capacity),
            "{app}"
        );
    }
    assert_eq!(broker.snapshot().workers["app-c"].rejected_capacity, 1);

    drop(a1);
    let _c = open(&broker, "app-c", &["t"], 100, HOUR);
    assert_eq!(broker.snapshot().total_connections, 4);
}

#[tokio::test(start_paused = true)]
async fn a_client_that_never_polls_is_dropped_by_the_heartbeat_and_the_others_continue() {
    let broker = broker(limits(100, 3, SECOND)).await;
    let mut stalled = open(&broker, "hub", &["t"], 3, HOUR);
    let mut reader = open(&broker, "hub", &["t"], 3, HOUR);
    let mut other_reader = open(&broker, "hub", &["t"], 3, HOUR);

    // 64 pings fill the queue exactly: still alive.
    for _ in 0..SSE_SUBSCRIBER_QUEUE_EVENTS {
        advance(SECOND).await;
        assert_eq!(frames_of(&mut reader).len(), 1, "the reader gets each ping");
        assert_eq!(frames_of(&mut other_reader).len(), 1);
    }
    assert_eq!(broker.snapshot().workers["hub"].disconnect_lagged, 0);

    // The 65th does not fit.
    advance(SECOND).await;
    assert_eq!(broker.snapshot().workers["hub"].disconnect_lagged, 1);
    let (_, ended) = drain(&mut reader);
    assert!(!ended, "the reader is unaffected");
    let (_, ended) = drain(&mut other_reader);
    assert!(!ended, "the other reader is unaffected");

    // The stalled stream ends without delivering the queued rest.
    assert_eq!(read(&mut stalled), Read::Ended);
    // Its slot is still held: only the Drop returns it.
    assert_eq!(broker.snapshot().total_connections, 3);
    assert_eq!(
        broker
            .subscribe(scope("hub"), topics(&["t"]), 3, HOUR)
            .err(),
        Some(SseSubscribeError::Capacity)
    );
    // It no longer takes part in the topic.
    broker.publish(&scope("hub"), vec![event("t", "after")]);
    assert_eq!(frames_of(&mut reader).len(), 1);

    drop(stalled);
    let stats = broker.snapshot();
    assert_eq!(stats.total_connections, 2);
    assert_eq!(stats.workers["hub"].disconnect_lagged, 1);
    assert_eq!(
        stats.workers["hub"].disconnect_client, 0,
        "an already retired subscriber is not counted again"
    );
    broker
        .subscribe(scope("hub"), topics(&["t"]), 3, HOUR)
        .expect("the slot is back");
}

#[tokio::test(start_paused = true)]
async fn queue_limit_in_events_is_exact_when_publishing() {
    let broker = broker(limits(100, 100, HOUR)).await;
    let mut stalled = open(&broker, "hub", &["t"], 100, HOUR);
    let mut reader = open(&broker, "hub", &["t"], 100, HOUR);

    for n in 0..SSE_SUBSCRIBER_QUEUE_EVENTS {
        broker.publish(&scope("hub"), vec![event("t", &n.to_string())]);
        assert_eq!(frames_of(&mut reader).len(), 1);
    }
    assert_eq!(broker.snapshot().workers["hub"].disconnect_lagged, 0);

    broker.publish(&scope("hub"), vec![event("t", "one-too-many")]);
    assert_eq!(broker.snapshot().workers["hub"].disconnect_lagged, 1);
    assert_eq!(frames_of(&mut reader).len(), 1, "the reader still gets it");
    assert_eq!(read(&mut stalled), Read::Ended);
}

#[tokio::test(start_paused = true)]
async fn queue_limit_in_bytes_is_enforced_with_large_events() {
    let broker = broker(limits(100, 100, HOUR)).await;
    let mut stalled = open(&broker, "hub", &["t"], 100, HOUR);
    let mut reader = open(&broker, "hub", &["t"], 100, HOUR);
    let big = "x".repeat(100_000);

    // Two frames are ~200 KB: under 256 KiB.
    for _ in 0..2 {
        broker.publish(&scope("hub"), vec![event("t", &big)]);
        assert_eq!(frames_of(&mut reader).len(), 1);
    }
    assert_eq!(broker.snapshot().workers["hub"].disconnect_lagged, 0);

    // The third would be ~300 KB queued: the stalled client goes.
    broker.publish(&scope("hub"), vec![event("t", &big)]);
    assert_eq!(broker.snapshot().workers["hub"].disconnect_lagged, 1);
    let frames = frames_of(&mut reader);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].len(), "data: \n\n".len() + 100_000);
    assert_eq!(read(&mut stalled), Read::Ended);
}

#[tokio::test(start_paused = true)]
async fn an_event_larger_than_a_whole_queue_is_dropped_without_evicting_anyone() {
    let broker = broker(limits(100, 100, HOUR)).await;
    let mut subscriber = open(&broker, "hub", &["t"], 100, HOUR);
    broker.publish(&scope("hub"), vec![event("t", &"y".repeat(300_000))]);
    assert_eq!(broker.snapshot().workers["hub"].disconnect_lagged, 0);
    assert_eq!(read(&mut subscriber), Read::Pending);
    broker.publish(&scope("hub"), vec![event("t", "small")]);
    assert_eq!(frames_of(&mut subscriber).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn heartbeat_reaches_a_reading_client_at_the_configured_interval() {
    let broker = broker(limits(8, 8, Duration::from_secs(2))).await;
    let mut subscriber = open(&broker, "hub", &["t"], 8, HOUR);

    advance(Duration::from_millis(1900)).await;
    assert_eq!(read(&mut subscriber), Read::Pending, "not before 2 s");
    advance(Duration::from_millis(200)).await;
    assert_eq!(read(&mut subscriber), Read::Frame(Bytes::from_static(PING)));
    assert_eq!(read(&mut subscriber), Read::Pending);

    advance(Duration::from_millis(1900)).await;
    assert_eq!(
        read(&mut subscriber),
        Read::Pending,
        "next one is 2 s later"
    );
    advance(Duration::from_millis(200)).await;
    assert_eq!(read(&mut subscriber), Read::Frame(Bytes::from_static(PING)));
}

#[tokio::test(start_paused = true)]
async fn ttl_sends_a_retry_with_jitter_then_ends_the_stream() {
    let broker = broker(limits(8, 8, SECOND)).await;
    let mut expiring = open(&broker, "hub", &["t"], 8, Duration::from_secs(10));
    let mut lasting = open(&broker, "hub", &["t"], 8, HOUR);

    let mut seen: Vec<Bytes> = Vec::new();
    for second in 1..=9 {
        advance(SECOND).await;
        let (frames, ended) = drain(&mut expiring);
        assert!(!ended, "still alive at {second}s");
        seen.extend(frames);
        let _ = frames_of(&mut lasting);
    }
    assert!(seen.iter().all(|frame| &frame[..] == PING));

    advance(SECOND).await;
    let (frames, ended) = drain(&mut expiring);
    assert!(ended, "the stream closes at the TTL tick");
    let retry = frames.last().expect("the retry frame is the last item");
    let text = std::str::from_utf8(retry).expect("utf8");
    let value: u64 = text
        .strip_prefix("retry: ")
        .and_then(|rest| rest.strip_suffix("\n\n"))
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("not a retry frame: {text:?}"));
    assert!((1000..=5000).contains(&value), "{value}");

    let stats = broker.snapshot();
    assert_eq!(stats.workers["hub"].disconnect_ttl, 1);
    assert_eq!(stats.workers["hub"].disconnect_lagged, 0);
    assert_eq!(stats.total_connections, 2, "the slot waits for the Drop");

    // The other subscriber, with a long TTL, is untouched.
    let (_, lasting_ended) = drain(&mut lasting);
    assert!(!lasting_ended);
    broker.publish(&scope("hub"), vec![event("t", "later")]);
    assert_eq!(frames_of(&mut lasting).len(), 1);
    assert_eq!(read(&mut expiring), Read::Ended);

    drop(expiring);
    let stats = broker.snapshot();
    assert_eq!(stats.total_connections, 1);
    assert_eq!(stats.workers["hub"].disconnect_client, 0);
}

#[tokio::test(start_paused = true)]
async fn a_ttl_that_lands_between_ticks_waits_for_the_next_tick() {
    let broker = broker(limits(8, 8, Duration::from_secs(4))).await;
    let mut subscriber = open(&broker, "hub", &["t"], 8, Duration::from_secs(5));
    advance(Duration::from_secs(4)).await;
    let (frames, ended) = drain(&mut subscriber);
    assert!(!ended);
    assert_eq!(frames.len(), 1, "a ping at 4 s");
    advance(Duration::from_secs(4)).await;
    let (frames, ended) = drain(&mut subscriber);
    assert!(ended, "revocation latency is at most ttl plus one tick");
    assert!(frames.last().expect("retry")[..].starts_with(b"retry: "));
}

#[tokio::test(start_paused = true)]
async fn drop_counts_a_client_disconnect_and_removes_the_empty_topic() {
    let broker = broker(limits(8, 8, HOUR)).await;
    let subscriber = open(&broker, "hub", &["a", "b"], 8, HOUR);
    assert_eq!(broker.snapshot().topics, 2);
    assert_eq!(broker.snapshot().total_connections, 1);

    drop(subscriber);
    let stats = broker.snapshot();
    assert_eq!(stats.topics, 0);
    assert_eq!(stats.total_connections, 0);
    assert_eq!(stats.workers["hub"].connections, 0);
    assert_eq!(stats.workers["hub"].disconnect_client, 1);
}

#[tokio::test(start_paused = true)]
async fn a_topic_survives_while_any_subscriber_remains() {
    let broker = broker(limits(8, 8, HOUR)).await;
    let first = open(&broker, "hub", &["t"], 8, HOUR);
    let mut second = open(&broker, "hub", &["t"], 8, HOUR);
    drop(first);
    assert_eq!(broker.snapshot().topics, 1);
    broker.publish(&scope("hub"), vec![event("t", "x")]);
    assert_eq!(frames_of(&mut second).len(), 1);
    drop(second);
    assert_eq!(broker.snapshot().topics, 0);
}

#[tokio::test(start_paused = true)]
async fn publishing_without_subscribers_is_a_silent_discard() {
    let broker = broker(limits(8, 8, HOUR)).await;
    broker.publish(&scope("nobody"), vec![event("t", "x"), event("u", "y")]);
    broker.publish(&scope("hub"), vec![]);
    let stats = broker.snapshot();
    assert_eq!(stats.workers["nobody"].published, 2);
    assert_eq!(stats.topics, 0);

    let mut subscriber = open(&broker, "hub", &["t"], 8, HOUR);
    broker.publish(&scope("hub"), vec![event("other", "x")]);
    assert_eq!(read(&mut subscriber), Read::Pending);
    assert_eq!(
        read(&mut subscriber),
        Read::Pending,
        "no replay either: nothing is queued for later"
    );
}

#[tokio::test(start_paused = true)]
async fn one_publish_in_two_topics_delivers_twice_and_a_repeated_topic_collapses() {
    let broker = broker(limits(8, 8, HOUR)).await;
    let mut both = open(&broker, "hub", &["a", "b"], 8, HOUR);
    let mut repeated = open(&broker, "hub", &["a", "a"], 8, HOUR);
    broker.publish(&scope("hub"), vec![event("a", "same"), event("b", "same")]);
    let frames = frames_of(&mut both);
    assert_eq!(
        frames.len(),
        2,
        "once per published topic, no deduplication"
    );
    assert_eq!(frames[0], frames[1]);
    assert_eq!(frames_of(&mut repeated).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn order_per_topic_is_the_call_order() {
    let broker = broker(limits(8, 8, HOUR)).await;
    let mut subscriber = open(&broker, "hub", &["t"], 8, HOUR);
    for n in 0..10 {
        broker.publish(&scope("hub"), vec![event("t", &n.to_string())]);
    }
    broker.publish(
        &scope("hub"),
        (10..20).map(|n| event("t", &n.to_string())).collect(),
    );
    let order: Vec<String> = frames_of(&mut subscriber)
        .iter()
        .map(|frame| String::from_utf8(frame.to_vec()).expect("utf8"))
        .collect();
    let expected: Vec<String> = (0..20).map(|n| format!("data: {n}\n\n")).collect();
    assert_eq!(order, expected);
}

#[tokio::test(start_paused = true)]
async fn shutdown_ends_streams_refuses_new_subscribers_and_aborts_the_tick() {
    let runtime = tokio::runtime::Handle::current();
    let baseline = runtime.metrics().num_alive_tasks();
    let broker = broker(limits(8, 8, SECOND)).await;
    assert_eq!(
        runtime.metrics().num_alive_tasks(),
        baseline + 1,
        "exactly one tick task exists for the whole broker"
    );
    let mut first = open(&broker, "hub", &["t"], 8, HOUR);
    let mut second = open(&broker, "other", &["u"], 8, HOUR);
    assert_eq!(
        runtime.metrics().num_alive_tasks(),
        baseline + 1,
        "subscribers add no task"
    );
    broker.publish(&scope("hub"), vec![event("t", "queued-before")]);

    broker.shutdown();
    settle().await;
    assert_eq!(
        read(&mut first),
        Read::Ended,
        "the queued rest is not delivered"
    );
    assert_eq!(read(&mut second), Read::Ended);
    assert_eq!(
        broker
            .subscribe(scope("hub"), topics(&["t"]), 8, HOUR)
            .err(),
        Some(SseSubscribeError::Unavailable)
    );
    assert_eq!(
        runtime.metrics().num_alive_tasks(),
        baseline,
        "tick aborted"
    );

    broker.publish(&scope("hub"), vec![event("t", "ignored")]);
    broker.shutdown();
    let stats = broker.snapshot();
    assert_eq!(stats.topics, 0);
    assert_eq!(stats.workers["hub"].disconnect_shutdown, 1);
    assert_eq!(stats.workers["other"].disconnect_shutdown, 1);
    assert_eq!(stats.workers["hub"].rejected_unavailable, 1);
    assert_eq!(
        stats.workers["hub"].published, 1,
        "nothing counted after shutdown"
    );

    drop((first, second));
    assert_eq!(broker.snapshot().total_connections, 0);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_broker_ends_the_streams_and_the_tick() {
    let runtime = tokio::runtime::Handle::current();
    let baseline = runtime.metrics().num_alive_tasks();
    let broker = broker(limits(8, 8, SECOND)).await;
    let mut subscriber = open(&broker, "hub", &["t"], 8, HOUR);
    drop(broker);
    settle().await;
    assert_eq!(read(&mut subscriber), Read::Ended);
    assert_eq!(runtime.metrics().num_alive_tasks(), baseline);
}

#[tokio::test(start_paused = true)]
async fn zero_max_connections_disables_the_broker_without_a_tick() {
    let runtime = tokio::runtime::Handle::current();
    let baseline = runtime.metrics().num_alive_tasks();
    let broker = broker(limits(0, 256, SECOND)).await;
    assert_eq!(
        runtime.metrics().num_alive_tasks(),
        baseline,
        "no tick task"
    );
    assert_eq!(
        broker
            .subscribe(scope("hub"), topics(&["t"]), 8, HOUR)
            .err(),
        Some(SseSubscribeError::Unavailable)
    );
    broker.publish(&scope("hub"), vec![event("t", "x")]);
    let stats = broker.snapshot();
    assert_eq!(stats.workers["hub"].rejected_unavailable, 1);
    assert_eq!(stats.workers["hub"].rejected_capacity, 0);
    assert_eq!(stats.total_connections, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_subscribe_publish_and_drop_leave_every_count_at_zero() {
    const TASKS: usize = 32;
    const ROUNDS: usize = 60;
    let broker = SseBroker::new(limits(1024, 8, HOUR));

    let mut handles = Vec::new();
    for task in 0..TASKS {
        let broker = std::sync::Arc::clone(&broker);
        handles.push(tokio::spawn(async move {
            let app = format!("app-{}", task % 4);
            let mut admitted = 0u64;
            let mut refused = 0u64;
            for round in 0..ROUNDS {
                let names = [format!("t{}", round % 3), "shared".to_string()];
                match broker.subscribe(scope(&app), names.to_vec(), 8, HOUR) {
                    Ok(mut subscription) => {
                        admitted += 1;
                        broker.publish(
                            &scope(&app),
                            vec![event("shared", "x"), event(&names[0], "y")],
                        );
                        if round % 2 == 0 {
                            let _ = subscription.next().now_or_never();
                        }
                        tokio::task::yield_now().await;
                        drop(subscription);
                    }
                    Err(SseSubscribeError::Capacity) => {
                        refused += 1;
                        broker.publish(&scope(&app), vec![event("shared", "z")]);
                    }
                    Err(SseSubscribeError::Unavailable) => panic!("broker is enabled"),
                }
            }
            (admitted, refused)
        }));
    }
    let mut admitted = 0;
    let mut refused = 0;
    for handle in handles {
        let (a, r) = handle.await.expect("task finished");
        admitted += a;
        refused += r;
    }

    let stats = broker.snapshot();
    assert_eq!(stats.total_connections, 0);
    assert_eq!(stats.topics, 0);
    let mut disconnects = 0;
    let mut rejected = 0;
    for (worker, worker_stats) in &stats.workers {
        assert_eq!(worker_stats.connections, 0, "{worker}");
        disconnects += worker_stats.disconnect_client
            + worker_stats.disconnect_lagged
            + worker_stats.disconnect_ttl
            + worker_stats.disconnect_shutdown;
        rejected += worker_stats.rejected_capacity;
    }
    assert_eq!(
        disconnects, admitted,
        "every admitted stream disconnected once"
    );
    assert_eq!(rejected, refused);
    assert_eq!(admitted + refused, (TASKS * ROUNDS) as u64);
    assert!(admitted > 0);
}

/// R66 #1: a subscriber removed as `lagged` stops holding its queue. A
/// second subscriber keeps one copy of the big frame; once the stalled one
/// is retired that copy must be the only owner left, with NO poll and NO
/// Drop of the retired stream (`try_into_mut` succeeds only when unique).
#[tokio::test(start_paused = true)]
async fn a_lagged_subscriber_releases_its_queue_without_a_poll_or_a_drop() {
    let broker = broker(limits(100, 100, HOUR)).await;
    let stalled = open(&broker, "hub", &["t"], 100, HOUR);
    let mut reader = open(&broker, "hub", &["t"], 100, HOUR);

    broker.publish(&scope("hub"), vec![event("t", &"b".repeat(100_000))]);
    let big = match read(&mut reader) {
        Read::Frame(frame) => frame,
        other => panic!("expected the big frame, got {other:?}"),
    };
    let big = match big.try_into_mut() {
        Ok(_) => panic!("the stalled subscriber still shares the frame"),
        Err(shared) => shared,
    };

    // 64 more fill the stalled queue (the big frame is the 1st), the 65th
    // retires it.
    for n in 0..SSE_SUBSCRIBER_QUEUE_EVENTS {
        broker.publish(&scope("hub"), vec![event("t", &n.to_string())]);
        assert_eq!(frames_of(&mut reader).len(), 1);
    }
    let stats = broker.snapshot();
    assert_eq!(stats.workers["hub"].disconnect_lagged, 1);
    assert_eq!(stats.total_connections, 2, "the slot stays until the Drop");

    assert!(
        big.try_into_mut().is_ok(),
        "the retired subscriber still owns a copy of the queued frame"
    );
    let stats = broker.snapshot();
    assert_eq!(stats.total_connections, 2);
    assert_eq!(stats.workers["hub"].connections, 2);
    drop(stalled);
    assert_eq!(broker.snapshot().total_connections, 1);
}

/// R66 #2: a subscriber retired by TTL with a pending queue is still
/// reachable by `shutdown`, which ends it and discards the queue without a
/// second disconnect count.
#[tokio::test(start_paused = true)]
async fn shutdown_reaches_a_ttl_retired_stream_with_a_pending_queue() {
    let broker = broker(limits(8, 8, SECOND)).await;
    let mut expiring = open(&broker, "hub", &["t"], 8, Duration::from_secs(3));
    for _ in 0..4 {
        advance(SECOND).await;
    }
    let stats = broker.snapshot();
    assert_eq!(stats.workers["hub"].disconnect_ttl, 1);
    assert_eq!(stats.total_connections, 1, "the slot waits for the Drop");

    broker.shutdown();
    assert_eq!(
        read(&mut expiring),
        Read::Ended,
        "the pending queue is discarded, not delivered"
    );
    let stats = broker.snapshot();
    assert_eq!(stats.workers["hub"].disconnect_ttl, 1);
    assert_eq!(stats.workers["hub"].disconnect_shutdown, 0);
    drop(expiring);
    assert_eq!(broker.snapshot().total_connections, 0);
}

#[tokio::test(start_paused = true)]
async fn dropping_the_broker_reaches_a_ttl_retired_stream_with_a_pending_queue() {
    let broker = broker(limits(8, 8, SECOND)).await;
    let mut expiring = open(&broker, "hub", &["t"], 8, Duration::from_secs(3));
    for _ in 0..4 {
        advance(SECOND).await;
    }
    drop(broker);
    assert_eq!(read(&mut expiring), Read::Ended);
}

/// TTL keeps delivering the queue and the `retry:` to a client that reads.
#[tokio::test(start_paused = true)]
async fn a_ttl_retired_stream_still_delivers_its_pending_queue_to_a_reader() {
    let broker = broker(limits(8, 8, SECOND)).await;
    let mut expiring = open(&broker, "hub", &["t"], 8, Duration::from_secs(3));
    for _ in 0..4 {
        advance(SECOND).await;
    }
    let (frames, ended) = drain(&mut expiring);
    assert!(ended);
    assert!(frames.len() >= 2);
    assert!(frames.last().expect("retry")[..].starts_with(b"retry: "));
    assert!(frames[..frames.len() - 1].iter().all(|f| &f[..] == PING));
}
