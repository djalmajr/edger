//! Gateway middleware tests (story 06.03).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use edger_core::{
    DurableSqlProvider, Extension, Middleware, RequestContext, SerializedRequest,
    SerializedResponse, StateValue,
};
use edger_ext_gateway::{
    GatewayCacheConfig, GatewayCorsConfig, GatewayExtension, GatewayProxyRule,
    GatewayRateLimitConfig, GatewayRedirectRule,
};
use edger_ext_turso_remote::RemoteTursoProvider;

fn local_upstream(response: &'static str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
        stream.write_all(response.as_bytes()).unwrap();
        request
    });
    (format!("http://{addr}"), handle)
}

#[test]
fn on_request_returns_none_continue() {
    let ext = GatewayExtension::new();
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/demo".into(),
        headers: vec![],
        body: None,
        request_id: "gw-test".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("gw-test");
    assert!(ext.on_request(&mut req, &ctx).unwrap().is_none());
}

#[test]
fn test_header_increments_invocation_count() {
    let ext = GatewayExtension::new();
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/demo".into(),
        headers: vec![("x-gateway-test".into(), "1".into())],
        body: None,
        request_id: "gw-test".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("gw-test");
    ext.on_request(&mut req, &ctx).unwrap();
    assert_eq!(ext.invocation_count(), 1);
}

#[test]
fn middleware_factory_returns_arc_dyn() {
    let mw = GatewayExtension::middleware();
    assert_eq!(mw.name(), "gateway");
    assert_eq!(mw.priority(), 0);
}

#[test]
fn cors_preflight_returns_no_content_with_allow_headers() {
    let ext = GatewayExtension::new().with_cors(GatewayCorsConfig {
        allowed_headers: vec!["authorization".into()],
        origin: "https://app.example.com".into(),
        ..Default::default()
    });
    let mut req = SerializedRequest {
        method: "OPTIONS".into(),
        uri: "/demo".into(),
        headers: vec![
            ("origin".into(), "https://app.example.com".into()),
            (
                "access-control-request-headers".into(),
                "authorization, content-type".into(),
            ),
        ],
        body: None,
        request_id: "gw-test".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("gw-test");

    let response = ext.on_request(&mut req, &ctx).unwrap().unwrap();

    assert_eq!(response.status, 204);
    assert!(response.body.is_none());
    assert!(response.headers.contains(&(
        "access-control-allow-origin".into(),
        "https://app.example.com".into()
    )));
    assert!(response.headers.contains(&(
        "access-control-allow-headers".into(),
        "authorization, content-type".into()
    )));
}

#[test]
fn cors_header_is_added_on_response() {
    let ext = GatewayExtension::new();
    let ctx = RequestContext::new("gw-test");
    let mut response = SerializedResponse {
        status: 200,
        headers: vec![],
        body: None,
    };

    ext.on_response(&mut response, &ctx);

    assert!(response
        .headers
        .contains(&("access-control-allow-origin".into(), "*".into())));
}

#[test]
fn redirect_rule_short_circuits_with_suffix_and_query() {
    let ext = GatewayExtension::new().with_redirect_rules(vec![GatewayRedirectRule::new(
        "/api",
        "https://backend.example.com/api",
    )]);
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users?active=1".into(),
        headers: vec![],
        body: None,
        request_id: "gw-test".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("gw-test");

    let response = ext.on_request(&mut req, &ctx).unwrap().unwrap();

    assert_eq!(response.status, 308);
    assert!(response.body.is_none());
    assert!(response.headers.contains(&(
        "location".into(),
        "https://backend.example.com/api/users?active=1".into()
    )));
}

#[test]
fn redirect_rule_matches_path_segments_only() {
    let ext = GatewayExtension::new().with_redirect_rules(vec![GatewayRedirectRule::new(
        "/api",
        "https://backend.example.com/api",
    )]);
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/apiary".into(),
        headers: vec![],
        body: None,
        request_id: "gw-test".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("gw-test");

    assert!(ext.on_request(&mut req, &ctx).unwrap().is_none());
}

#[test]
fn cors_preflight_wins_over_redirect_rule() {
    let ext = GatewayExtension::new().with_redirect_rules(vec![GatewayRedirectRule::new(
        "/api",
        "https://backend.example.com/api",
    )]);
    let mut req = SerializedRequest {
        method: "OPTIONS".into(),
        uri: "/api/users".into(),
        headers: vec![("origin".into(), "https://app.example.com".into())],
        body: None,
        request_id: "gw-test".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("gw-test");

    let response = ext.on_request(&mut req, &ctx).unwrap().unwrap();

    assert_eq!(response.status, 204);
    assert!(response
        .headers
        .iter()
        .all(|(name, _)| !name.eq_ignore_ascii_case("location")));
}

#[test]
fn proxy_rule_forwards_to_local_upstream_without_sensitive_headers() {
    let (target, handle) = local_upstream(
        "HTTP/1.1 201 Created\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
    );
    let ext = GatewayExtension::new().with_proxy_rules(vec![GatewayProxyRule::try_new(
        "/api",
        format!("{target}/v1"),
    )
    .unwrap()]);
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users?active=1".into(),
        headers: vec![
            ("authorization".into(), "Bearer should-not-forward".into()),
            ("x-request-id".into(), "proxy-test".into()),
        ],
        body: None,
        request_id: "proxy-test".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("proxy-test");

    let response = ext.on_request(&mut req, &ctx).unwrap().unwrap();
    let upstream_request = handle.join().unwrap();

    assert_eq!(response.status, 201);
    assert_eq!(response.body.as_deref(), Some(&b"{\"ok\":true}"[..]));
    assert!(response
        .headers
        .contains(&("content-type".into(), "application/json".into())));
    assert!(upstream_request.starts_with("GET /v1/users?active=1 HTTP/1.1"));
    assert!(upstream_request.contains("x-request-id: proxy-test"));
    assert!(!upstream_request.contains("authorization"));
    assert!(!upstream_request.contains("should-not-forward"));

    let diagnostics = ext.diagnostics().unwrap();
    assert_eq!(diagnostics["requests"]["proxied"], 1);
    assert_eq!(diagnostics["config"]["proxyRules"]["count"], 1);
}

#[test]
fn proxy_rule_rejects_non_local_targets() {
    let err = GatewayProxyRule::try_new("/api", "http://example.com").unwrap_err();

    assert_eq!(err.code, "GATEWAY_PROXY_TARGET_DENIED");
}

#[test]
fn rate_limit_blocks_after_capacity_with_operational_headers() {
    let ext = GatewayExtension::new().with_rate_limit(GatewayRateLimitConfig::new(2, 60));
    let ctx = RequestContext::new("gw-test");

    let mut first = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users".into(),
        headers: vec![("x-forwarded-for".into(), "203.0.113.7".into())],
        body: None,
        request_id: "gw-test-1".into(),
        base_href: None,
    };
    let mut second = first.clone();
    second.request_id = "gw-test-2".into();
    let mut third = first.clone();
    third.request_id = "gw-test-3".into();

    assert!(ext.on_request(&mut first, &ctx).unwrap().is_none());
    assert!(ext.on_request(&mut second, &ctx).unwrap().is_none());

    let response = ext.on_request(&mut third, &ctx).unwrap().unwrap();

    assert_eq!(response.status, 429);
    assert!(response.body.is_none());
    assert!(response
        .headers
        .contains(&("x-ratelimit-limit".into(), "2".into())));
    assert!(response
        .headers
        .contains(&("x-ratelimit-remaining".into(), "0".into())));
    assert!(response
        .headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("retry-after") && value == "30"));
}

#[test]
fn rate_limit_uses_independent_buckets_per_client() {
    let ext = GatewayExtension::new().with_rate_limit(GatewayRateLimitConfig::new(1, 60));
    let ctx = RequestContext::new("gw-test");
    let mut first_client = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users".into(),
        headers: vec![("x-forwarded-for".into(), "203.0.113.7".into())],
        body: None,
        request_id: "gw-test-1".into(),
        base_href: None,
    };
    let mut second_client = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users".into(),
        headers: vec![("x-forwarded-for".into(), "198.51.100.10".into())],
        body: None,
        request_id: "gw-test-2".into(),
        base_href: None,
    };
    let mut first_client_again = first_client.clone();
    first_client_again.request_id = "gw-test-3".into();

    assert!(ext.on_request(&mut first_client, &ctx).unwrap().is_none());
    assert!(ext.on_request(&mut second_client, &ctx).unwrap().is_none());

    let response = ext
        .on_request(&mut first_client_again, &ctx)
        .unwrap()
        .unwrap();

    assert_eq!(response.status, 429);
}

#[test]
fn cors_preflight_does_not_consume_rate_limit_bucket() {
    let ext = GatewayExtension::new().with_rate_limit(GatewayRateLimitConfig::new(1, 60));
    let ctx = RequestContext::new("gw-test");
    let mut preflight = SerializedRequest {
        method: "OPTIONS".into(),
        uri: "/api/users".into(),
        headers: vec![
            ("origin".into(), "https://app.example.com".into()),
            ("x-forwarded-for".into(), "203.0.113.7".into()),
        ],
        body: None,
        request_id: "gw-test-preflight".into(),
        base_href: None,
    };
    let mut get = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users".into(),
        headers: vec![("x-forwarded-for".into(), "203.0.113.7".into())],
        body: None,
        request_id: "gw-test-get".into(),
        base_href: None,
    };

    let preflight_response = ext.on_request(&mut preflight, &ctx).unwrap().unwrap();

    assert_eq!(preflight_response.status, 204);
    assert!(ext.on_request(&mut get, &ctx).unwrap().is_none());
}

#[test]
fn rate_limit_runs_before_redirect_rules() {
    let ext = GatewayExtension::new()
        .with_rate_limit(GatewayRateLimitConfig::new(1, 60))
        .with_redirect_rules(vec![GatewayRedirectRule::new(
            "/api",
            "https://backend.example.com/api",
        )]);
    let ctx = RequestContext::new("gw-test");
    let mut first = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users".into(),
        headers: vec![("x-forwarded-for".into(), "203.0.113.7".into())],
        body: None,
        request_id: "gw-test-1".into(),
        base_href: None,
    };
    let mut second = first.clone();
    second.request_id = "gw-test-2".into();

    let redirect = ext.on_request(&mut first, &ctx).unwrap().unwrap();
    let blocked = ext.on_request(&mut second, &ctx).unwrap().unwrap();

    assert_eq!(redirect.status, 308);
    assert_eq!(blocked.status, 429);
    assert!(blocked
        .headers
        .iter()
        .all(|(name, _)| !name.eq_ignore_ascii_case("location")));
}

#[test]
fn diagnostics_tracks_gateway_decisions_without_sensitive_data() {
    let ext = GatewayExtension::new()
        .with_rate_limit(GatewayRateLimitConfig::new(1, 60))
        .with_redirect_rules(vec![GatewayRedirectRule::new(
            "/api",
            "https://backend.example.com/api",
        )]);
    let ctx = RequestContext::new("gw-test");
    let mut continued = SerializedRequest {
        method: "GET".into(),
        uri: "/plain".into(),
        headers: vec![
            ("x-forwarded-for".into(), "203.0.113.1".into()),
            ("authorization".into(), "Bearer should-not-leak".into()),
        ],
        body: None,
        request_id: "gw-continue".into(),
        base_href: None,
    };
    let mut preflight = SerializedRequest {
        method: "OPTIONS".into(),
        uri: "/api/users".into(),
        headers: vec![
            ("origin".into(), "https://app.example.com".into()),
            ("x-forwarded-for".into(), "203.0.113.2".into()),
        ],
        body: None,
        request_id: "gw-preflight".into(),
        base_href: None,
    };
    let mut redirected = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users".into(),
        headers: vec![("x-forwarded-for".into(), "203.0.113.3".into())],
        body: None,
        request_id: "gw-redirect".into(),
        base_href: None,
    };
    let mut blocked = redirected.clone();
    blocked.request_id = "gw-blocked".into();

    assert!(ext.on_request(&mut continued, &ctx).unwrap().is_none());
    assert_eq!(
        ext.on_request(&mut preflight, &ctx)
            .unwrap()
            .unwrap()
            .status,
        204
    );
    assert_eq!(
        ext.on_request(&mut redirected, &ctx)
            .unwrap()
            .unwrap()
            .status,
        308
    );
    assert_eq!(
        ext.on_request(&mut blocked, &ctx).unwrap().unwrap().status,
        429
    );

    let diagnostics = ext.diagnostics().unwrap();

    assert_eq!(diagnostics["requests"]["total"], 4);
    assert_eq!(diagnostics["requests"]["continued"], 1);
    assert_eq!(diagnostics["requests"]["preflight"], 1);
    assert_eq!(diagnostics["requests"]["redirected"], 1);
    assert_eq!(diagnostics["requests"]["rateLimited"], 1);
    assert_eq!(diagnostics["config"]["cors"]["origin"], "*");
    assert_eq!(diagnostics["config"]["redirectRules"]["count"], 1);
    assert_eq!(diagnostics["config"]["rateLimit"]["enabled"], true);
    assert_eq!(diagnostics["config"]["rateLimit"]["maxRequests"], 1);
    assert_eq!(diagnostics["rateLimit"]["enabled"], true);
    assert_eq!(diagnostics["rateLimit"]["activeBuckets"], 2);
    assert_eq!(diagnostics["recentDecisions"].as_array().unwrap().len(), 4);
    assert_eq!(diagnostics["recentDecisions"][0]["decision"], "continue");
    assert_eq!(diagnostics["recentDecisions"][1]["decision"], "preflight");
    assert_eq!(diagnostics["recentDecisions"][2]["decision"], "redirect");
    assert_eq!(
        diagnostics["recentDecisions"][3]["decision"],
        "rate_limited"
    );
    assert_eq!(diagnostics["recentDecisions"][3]["status"], 429);
    assert_eq!(diagnostics["recentDecisions"][3]["rateLimited"], true);

    let body = diagnostics.to_string();
    assert!(!body.contains("authorization"));
    assert!(!body.contains("should-not-leak"));
}

#[test]
fn diagnostics_records_response_duration_without_sensitive_data() {
    let ext = GatewayExtension::new();
    let ctx = RequestContext::new("gw-duration");
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/plain".into(),
        headers: vec![("authorization".into(), "Bearer should-not-leak".into())],
        body: None,
        request_id: "gw-duration".into(),
        base_href: None,
    };
    let mut response = SerializedResponse {
        status: 202,
        headers: vec![],
        body: Some(b"accepted".to_vec().into()),
    };

    assert!(ext.on_request(&mut req, &ctx).unwrap().is_none());
    ext.on_response(&mut response, &ctx);

    let diagnostics = ext.diagnostics().unwrap();
    let entry = &diagnostics["recentDecisions"][0];

    assert_eq!(entry["requestId"], "gw-duration");
    assert_eq!(entry["decision"], "continue");
    assert_eq!(entry["status"], 202);
    assert!(entry["durationMs"].is_u64());
    assert!(!diagnostics.to_string().contains("authorization"));
    assert!(!diagnostics.to_string().contains("should-not-leak"));
    assert!(!diagnostics.to_string().contains("accepted"));
}

#[test]
fn persistent_history_uses_external_durable_sql_provider() {
    let temp = tempfile::tempdir().unwrap();
    let sql = Arc::new(
        RemoteTursoProvider::new_local_for_tests(vec![(
            "@gateway".to_string(),
            temp.path().join("gateway.db"),
        )])
        .unwrap(),
    );
    let ext = GatewayExtension::new().with_history_store(sql.clone(), "@gateway");
    let ctx = RequestContext::new("gw-persistent");
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/plain?token=should-not-persist".into(),
        headers: vec![
            ("authorization".into(), "Bearer should-not-leak".into()),
            ("x-forwarded-for".into(), "203.0.113.9".into()),
        ],
        body: None,
        request_id: "gw-persistent".into(),
        base_href: None,
    };
    let mut response = SerializedResponse {
        status: 202,
        headers: vec![],
        body: Some(b"accepted".to_vec().into()),
    };

    assert!(ext.on_request(&mut req, &ctx).unwrap().is_none());
    ext.on_response(&mut response, &ctx);

    let diagnostics = ext.diagnostics().unwrap();
    assert_eq!(ext.persistent_decision_count().unwrap(), 1);
    assert_eq!(diagnostics["history"]["persistent"]["enabled"], true);
    assert_eq!(diagnostics["history"]["persistent"]["decisions"], 1);

    let rows = sql
        .query(
            "@gateway",
            "select request_id, decision, path, status, rate_limited, duration_ms, client from gateway_decisions",
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], StateValue::Text("gw-persistent".into()));
    assert_eq!(rows[0].values[1], StateValue::Text("continue".into()));
    assert_eq!(rows[0].values[2], StateValue::Text("/plain".into()));
    assert_eq!(rows[0].values[3], StateValue::Integer(202));
    assert_eq!(rows[0].values[4], StateValue::Integer(0));
    assert!(matches!(rows[0].values[5], StateValue::Integer(_)));
    assert_eq!(rows[0].values[6], StateValue::Text("203.0.113.9".into()));

    let persisted = format!("{:?}", rows);
    assert!(!persisted.contains("authorization"));
    assert!(!persisted.contains("should-not-leak"));
    assert!(!persisted.contains("accepted"));
    assert!(!persisted.contains("should-not-persist"));
}

#[test]
fn durable_cache_records_hit_miss_and_redacts_cache_key() {
    let temp = tempfile::tempdir().unwrap();
    let sql = Arc::new(
        RemoteTursoProvider::new_local_for_tests(vec![(
            "@gateway".to_string(),
            temp.path().join("gateway-cache.db"),
        )])
        .unwrap(),
    );
    let ext = GatewayExtension::new().with_cache_store(
        GatewayCacheConfig::new(60),
        sql.clone(),
        "@gateway",
    );
    let mut first = SerializedRequest {
        method: "GET".into(),
        uri: "/public?token=should-not-persist".into(),
        headers: vec![("host".into(), "App.Example.Test:19080".into())],
        body: None,
        request_id: "gw-cache-1".into(),
        base_href: None,
    };
    let mut first_response = SerializedResponse {
        status: 200,
        headers: vec![("content-type".into(), "text/plain".into())],
        body: Some(b"cached-public".to_vec().into()),
    };
    let mut second = first.clone();
    second.request_id = "gw-cache-2".into();

    assert!(ext
        .on_request(&mut first, &RequestContext::new("gw-cache-1"))
        .unwrap()
        .is_none());
    ext.on_response(&mut first_response, &RequestContext::new("gw-cache-1"));
    assert!(first_response
        .headers
        .contains(&("x-edger-cache".into(), "miss".into())));

    let cached = ext
        .on_request(&mut second, &RequestContext::new("gw-cache-2"))
        .unwrap()
        .unwrap();

    assert_eq!(cached.status, 200);
    assert_eq!(cached.body.as_deref(), Some(&b"cached-public"[..]));
    assert!(cached
        .headers
        .contains(&("x-edger-cache".into(), "hit".into())));

    let diagnostics = ext.diagnostics().unwrap();
    assert_eq!(diagnostics["cache"]["enabled"], true);
    assert_eq!(diagnostics["cache"]["hits"], 1);
    assert_eq!(diagnostics["cache"]["misses"], 1);
    assert_eq!(diagnostics["cache"]["writes"], 1);
    assert_eq!(diagnostics["cache"]["activeEntries"], 1);
    assert_eq!(diagnostics["requests"]["cacheHit"], 1);
    assert_eq!(diagnostics["recentDecisions"][1]["decision"], "cache_hit");

    let rows = sql
        .query(
            "@gateway",
            "select cache_key, headers_json, body from gateway_cache_entries",
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    let StateValue::Text(cache_key) = &rows[0].values[0] else {
        panic!("cache key should be stored as text");
    };
    assert_eq!(cache_key.len(), 64);
    assert!(!cache_key.contains("public"));
    assert!(!cache_key.contains("should-not-persist"));

    let persisted = format!("{:?}", rows);
    assert!(!persisted.contains("App.Example.Test"));
    assert!(!persisted.contains("should-not-persist"));
}

#[test]
fn durable_cache_ttl_expiry_is_observable() {
    let temp = tempfile::tempdir().unwrap();
    let sql = Arc::new(
        RemoteTursoProvider::new_local_for_tests(vec![(
            "@gateway".to_string(),
            temp.path().join("gateway-cache-ttl.db"),
        )])
        .unwrap(),
    );
    let ext = GatewayExtension::new().with_cache_store(
        GatewayCacheConfig::new(0),
        sql.clone(),
        "@gateway",
    );
    let mut first = SerializedRequest {
        method: "GET".into(),
        uri: "/ttl".into(),
        headers: vec![("host".into(), "app.example.test".into())],
        body: None,
        request_id: "gw-cache-ttl-1".into(),
        base_href: None,
    };
    let mut response = SerializedResponse {
        status: 200,
        headers: vec![],
        body: Some(b"expires".to_vec().into()),
    };
    let mut second = first.clone();
    second.request_id = "gw-cache-ttl-2".into();

    assert!(ext
        .on_request(&mut first, &RequestContext::new("gw-cache-ttl-1"))
        .unwrap()
        .is_none());
    ext.on_response(&mut response, &RequestContext::new("gw-cache-ttl-1"));
    assert!(ext
        .on_request(&mut second, &RequestContext::new("gw-cache-ttl-2"))
        .unwrap()
        .is_none());

    let diagnostics = ext.diagnostics().unwrap();
    assert_eq!(diagnostics["cache"]["misses"], 2);
    assert_eq!(diagnostics["cache"]["expired"], 1);
    assert_eq!(diagnostics["cache"]["activeEntries"], 0);
}

#[test]
fn durable_cache_skips_sensitive_requests() {
    let temp = tempfile::tempdir().unwrap();
    let sql = Arc::new(
        RemoteTursoProvider::new_local_for_tests(vec![(
            "@gateway".to_string(),
            temp.path().join("gateway-cache-sensitive.db"),
        )])
        .unwrap(),
    );
    let ext =
        GatewayExtension::new().with_cache_store(GatewayCacheConfig::new(60), sql, "@gateway");
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/private".into(),
        headers: vec![("authorization".into(), "Bearer should-not-cache".into())],
        body: None,
        request_id: "gw-cache-sensitive".into(),
        base_href: None,
    };
    let mut response = SerializedResponse {
        status: 200,
        headers: vec![],
        body: Some(b"private".to_vec().into()),
    };

    assert!(ext
        .on_request(&mut req, &RequestContext::new("gw-cache-sensitive"))
        .unwrap()
        .is_none());
    ext.on_response(&mut response, &RequestContext::new("gw-cache-sensitive"));

    let diagnostics = ext.diagnostics().unwrap();
    assert_eq!(diagnostics["cache"]["hits"], 0);
    assert_eq!(diagnostics["cache"]["misses"], 0);
    assert_eq!(diagnostics["cache"]["writes"], 0);
    assert_eq!(diagnostics["cache"]["activeEntries"], 0);
    assert!(response
        .headers
        .iter()
        .all(|(name, _)| !name.eq_ignore_ascii_case("x-edger-cache")));
}

#[test]
fn persistent_rate_limit_survives_gateway_reconstruction() {
    let temp = tempfile::tempdir().unwrap();
    let sql = Arc::new(
        RemoteTursoProvider::new_local_for_tests(vec![(
            "@gateway".to_string(),
            temp.path().join("gateway-rate-limit.db"),
        )])
        .unwrap(),
    );
    let config = GatewayRateLimitConfig::new(1, 60).with_key_header("x-client-id");
    let first_ext = GatewayExtension::new().with_persistent_rate_limit_store(
        config.clone(),
        sql.clone(),
        "@gateway",
    );
    let second_ext =
        GatewayExtension::new().with_persistent_rate_limit_store(config, sql.clone(), "@gateway");
    let mut first = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users".into(),
        headers: vec![("x-client-id".into(), "team-a".into())],
        body: None,
        request_id: "gw-persistent-rl-1".into(),
        base_href: None,
    };
    let mut second = first.clone();
    second.request_id = "gw-persistent-rl-2".into();

    assert!(first_ext
        .on_request(&mut first, &RequestContext::new("gw-persistent-rl-1"))
        .unwrap()
        .is_none());

    let blocked = second_ext
        .on_request(&mut second, &RequestContext::new("gw-persistent-rl-2"))
        .unwrap()
        .unwrap();

    assert_eq!(blocked.status, 429);
    assert!(blocked
        .headers
        .contains(&("x-ratelimit-limit".into(), "1".into())));
    assert!(blocked
        .headers
        .contains(&("x-ratelimit-remaining".into(), "0".into())));

    let diagnostics = second_ext.diagnostics().unwrap();
    assert_eq!(diagnostics["rateLimit"]["enabled"], true);
    assert_eq!(diagnostics["rateLimit"]["mode"], "persistent");
    assert_eq!(diagnostics["rateLimit"]["activeBuckets"], 1);

    let rows = sql
        .query(
            "@gateway",
            "select bucket_key, request_count from gateway_rate_limit_buckets",
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[1], StateValue::Integer(1));
    let persisted = format!("{:?}", rows);
    assert!(!persisted.contains("team-a"));
}

#[test]
fn memory_rate_limit_remains_local_without_provider() {
    let config = GatewayRateLimitConfig::new(1, 60).with_key_header("x-client-id");
    let first_ext = GatewayExtension::new().with_rate_limit(config.clone());
    let second_ext = GatewayExtension::new().with_rate_limit(config);
    let mut first = SerializedRequest {
        method: "GET".into(),
        uri: "/api/users".into(),
        headers: vec![("x-client-id".into(), "team-a".into())],
        body: None,
        request_id: "gw-memory-rl-1".into(),
        base_href: None,
    };
    let mut second = first.clone();
    second.request_id = "gw-memory-rl-2".into();

    assert!(first_ext
        .on_request(&mut first, &RequestContext::new("gw-memory-rl-1"))
        .unwrap()
        .is_none());
    assert!(second_ext
        .on_request(&mut second, &RequestContext::new("gw-memory-rl-2"))
        .unwrap()
        .is_none());

    let diagnostics = second_ext.diagnostics().unwrap();
    assert_eq!(diagnostics["rateLimit"]["enabled"], true);
    assert_eq!(diagnostics["rateLimit"]["mode"], "memory");
    assert_eq!(diagnostics["rateLimit"]["activeBuckets"], 1);
}

#[test]
fn diagnostics_keeps_only_recent_gateway_decisions() {
    let ext = GatewayExtension::new();
    let ctx = RequestContext::new("gw-test");

    for index in 0..105 {
        let mut req = SerializedRequest {
            method: "GET".into(),
            uri: format!("/plain/{index}"),
            headers: vec![],
            body: None,
            request_id: format!("gw-{index}"),
            base_href: None,
        };
        assert!(ext.on_request(&mut req, &ctx).unwrap().is_none());
    }

    let diagnostics = ext.diagnostics().unwrap();
    let decisions = diagnostics["recentDecisions"].as_array().unwrap();

    assert_eq!(diagnostics["requests"]["total"], 105);
    assert_eq!(decisions.len(), 100);
    assert_eq!(decisions[0]["requestId"], "gw-5");
    assert_eq!(decisions[99]["requestId"], "gw-104");
}
