//! edger-ext-gateway — Middleware template extension (Epic 06.03).
//!
//! Copy this crate to scaffold new `edger-ext-*` middleware. Implements **only**
//! `Middleware` (choose ONE — do not add `AuthProvider` here).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use edger_core::{
    CoreError, DurableSqlProvider, Extension, ExtensionCapability, ExtensionContext, Middleware,
    RequestContext, SerializedRequest, SerializedResponse, StateValue,
};
use serde_json::{json, Value};
use tracing::trace;

const GATEWAY_TEST_HEADER: &str = "x-gateway-test";
const DEFAULT_DECISION_LOG_CAPACITY: usize = 100;
const DEFAULT_REDIRECT_STATUS: u16 = 308;
const RATE_LIMIT_LIMIT_HEADER: &str = "x-ratelimit-limit";
const RATE_LIMIT_REMAINING_HEADER: &str = "x-ratelimit-remaining";
const RETRY_AFTER_HEADER: &str = "retry-after";
const GATEWAY_HISTORY_ERROR: &str = "GATEWAY_HISTORY_ERROR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayCorsConfig {
    pub allowed_headers: Vec<String>,
    pub max_age_seconds: u32,
    pub methods: Vec<String>,
    pub origin: String,
}

impl Default for GatewayCorsConfig {
    fn default() -> Self {
        Self {
            allowed_headers: vec![],
            max_age_seconds: 86_400,
            methods: vec![
                "GET".into(),
                "HEAD".into(),
                "PUT".into(),
                "PATCH".into(),
                "POST".into(),
                "DELETE".into(),
                "OPTIONS".into(),
            ],
            origin: "*".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayRedirectRule {
    pub from_prefix: String,
    pub status: u16,
    pub target: String,
}

impl GatewayRedirectRule {
    pub fn new(from_prefix: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            from_prefix: Self::normalize_prefix(from_prefix.into()),
            status: DEFAULT_REDIRECT_STATUS,
            target: target.into(),
        }
    }

    pub fn with_status(mut self, status: u16) -> Self {
        self.status = if Self::is_redirect_status(status) {
            status
        } else {
            DEFAULT_REDIRECT_STATUS
        };
        self
    }

    fn is_redirect_status(status: u16) -> bool {
        matches!(status, 301 | 302 | 307 | 308)
    }

    fn normalize_prefix(prefix: String) -> String {
        let trimmed = prefix.trim();
        let absolute = if trimmed.starts_with('/') {
            trimmed.to_string()
        } else {
            format!("/{trimmed}")
        };
        if absolute.len() == 1 {
            absolute
        } else {
            absolute.trim_end_matches('/').to_string()
        }
    }

    fn location_for(&self, uri: &str) -> Option<String> {
        let (path, query) = split_path_query(uri);
        let suffix = self.match_suffix(path)?;
        Some(self.build_location(suffix, query))
    }

    fn match_suffix<'a>(&self, path: &'a str) -> Option<&'a str> {
        if self.from_prefix == "/" {
            return Some(path);
        }
        if path == self.from_prefix {
            return Some("");
        }
        path.strip_prefix(&self.from_prefix)
            .filter(|suffix| suffix.starts_with('/'))
    }

    fn build_location(&self, suffix: &str, query: Option<&str>) -> String {
        let mut location = if suffix.is_empty() {
            self.target.clone()
        } else if self.target.ends_with('/') && suffix.starts_with('/') {
            format!("{}{}", self.target.trim_end_matches('/'), suffix)
        } else if !self.target.ends_with('/') && !suffix.starts_with('/') {
            format!("{}/{}", self.target, suffix)
        } else {
            format!("{}{}", self.target, suffix)
        };
        if let Some(query) = query {
            location.push('?');
            location.push_str(query);
        }
        location
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayRateLimitConfig {
    pub key_header: Option<String>,
    pub max_requests: u32,
    pub window_seconds: u64,
}

impl GatewayRateLimitConfig {
    pub fn new(max_requests: u32, window_seconds: u64) -> Self {
        Self {
            key_header: None,
            max_requests: max_requests.max(1),
            window_seconds: window_seconds.max(1),
        }
    }

    pub fn with_key_header(mut self, header: impl Into<String>) -> Self {
        let header = header.into();
        self.key_header = (!header.trim().is_empty()).then_some(header);
        self
    }
}

#[derive(Debug)]
struct GatewayRateLimit {
    buckets: Mutex<HashMap<String, RateLimitBucket>>,
    config: GatewayRateLimitConfig,
}

impl GatewayRateLimit {
    fn new(config: GatewayRateLimitConfig) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            config,
        }
    }

    fn active_bucket_count(&self) -> usize {
        self.buckets
            .lock()
            .map(|buckets| buckets.len())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
struct RateLimitBucket {
    last_refill: Instant,
    tokens: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RateLimitDecision {
    allowed: bool,
    remaining: u32,
    retry_after_seconds: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct GatewayDecisionCounters {
    continued: u64,
    preflight: u64,
    rate_limited: u64,
    redirected: u64,
    total: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GatewayDecisionLogEntry {
    client: String,
    decision: &'static str,
    duration_ms: Option<u64>,
    method: String,
    path: String,
    rate_limited: bool,
    request_id: String,
    status: Option<u16>,
}

impl GatewayDecisionLogEntry {
    fn as_json(&self) -> Value {
        json!({
            "client": self.client,
            "decision": self.decision,
            "durationMs": self.duration_ms,
            "method": self.method,
            "path": self.path,
            "rateLimited": self.rate_limited,
            "requestId": self.request_id,
            "status": self.status,
        })
    }
}

struct GatewayHistoryStore {
    namespace: String,
    sql: Arc<dyn DurableSqlProvider>,
}

impl GatewayHistoryStore {
    fn new(sql: Arc<dyn DurableSqlProvider>, namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            sql,
        }
    }

    fn ensure_schema(&self) -> Result<(), CoreError> {
        self.sql.execute_batch(
            &self.namespace,
            r#"
            create table if not exists gateway_decisions (
                id integer primary key autoincrement,
                request_id text not null,
                method text not null,
                path text not null,
                decision text not null,
                status integer,
                rate_limited integer not null,
                duration_ms integer,
                client text not null,
                created_at_ms integer not null
            );
            create index if not exists idx_gateway_decisions_request_id
                on gateway_decisions(request_id);
            "#,
        )
    }

    fn record(&self, entry: &GatewayDecisionLogEntry) -> Result<(), CoreError> {
        self.ensure_schema()?;
        let params = vec![
            StateValue::Text(entry.request_id.clone()),
            StateValue::Text(entry.method.clone()),
            StateValue::Text(entry.path.clone()),
            StateValue::Text(entry.decision.to_string()),
            optional_u16(entry.status),
            StateValue::Integer(if entry.rate_limited { 1 } else { 0 }),
            optional_u64(entry.duration_ms),
            StateValue::Text(entry.client.clone()),
            StateValue::Integer(now_millis()),
        ];
        self.sql.execute(
            &self.namespace,
            r#"
            insert into gateway_decisions (
                request_id,
                method,
                path,
                decision,
                status,
                rate_limited,
                duration_ms,
                client,
                created_at_ms
            ) values (?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
            &params,
        )?;
        Ok(())
    }

    fn complete_response(
        &self,
        request_id: &str,
        status: u16,
        duration_ms: u64,
    ) -> Result<(), CoreError> {
        self.ensure_schema()?;
        let params = vec![
            StateValue::Integer(i64::from(status)),
            optional_u64(Some(duration_ms)),
            StateValue::Text(request_id.to_string()),
        ];
        self.sql.execute(
            &self.namespace,
            r#"
            update gateway_decisions
               set status = ?,
                   duration_ms = ?
             where id = (
                select id
                  from gateway_decisions
                 where request_id = ?
                 order by id desc
                 limit 1
             )
            "#,
            &params,
        )?;
        Ok(())
    }

    fn decision_count(&self) -> Result<u64, CoreError> {
        self.ensure_schema()?;
        let rows = self.sql.query(
            &self.namespace,
            "select count(*) as total from gateway_decisions",
            &[],
        )?;
        let Some(row) = rows.first() else {
            return Err(CoreError::new(
                GATEWAY_HISTORY_ERROR,
                "gateway history count returned no rows",
            ));
        };
        let Some(StateValue::Integer(total)) = row.values.first() else {
            return Err(CoreError::new(
                GATEWAY_HISTORY_ERROR,
                "gateway history count returned an unexpected value",
            ));
        };
        u64::try_from(*total).map_err(|_| {
            CoreError::new(
                GATEWAY_HISTORY_ERROR,
                "gateway history count returned a negative value",
            )
        })
    }
}

#[derive(Debug)]
struct GatewayDiagnostics {
    capacity: usize,
    counters: Mutex<GatewayDecisionCounters>,
    recent_decisions: Mutex<VecDeque<GatewayDecisionLogEntry>>,
}

impl Default for GatewayDiagnostics {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_DECISION_LOG_CAPACITY,
            counters: Mutex::new(GatewayDecisionCounters::default()),
            recent_decisions: Mutex::new(VecDeque::with_capacity(DEFAULT_DECISION_LOG_CAPACITY)),
        }
    }
}

impl GatewayDiagnostics {
    fn record(&self, entry: GatewayDecisionLogEntry) {
        if let Ok(mut counters) = self.counters.lock() {
            counters.total = counters.total.saturating_add(1);
            match entry.decision {
                "continue" => counters.continued = counters.continued.saturating_add(1),
                "preflight" => counters.preflight = counters.preflight.saturating_add(1),
                "rate_limited" => {
                    counters.rate_limited = counters.rate_limited.saturating_add(1);
                }
                "redirect" => counters.redirected = counters.redirected.saturating_add(1),
                _ => {}
            }
        }

        if let Ok(mut recent_decisions) = self.recent_decisions.lock() {
            if recent_decisions.len() == self.capacity {
                recent_decisions.pop_front();
            }
            recent_decisions.push_back(entry);
        }
    }

    fn complete_response(&self, request_id: &str, status: u16, duration_ms: u64) {
        if let Ok(mut recent_decisions) = self.recent_decisions.lock() {
            if let Some(entry) = recent_decisions
                .iter_mut()
                .rev()
                .find(|entry| entry.request_id == request_id)
            {
                entry.status = Some(status);
                entry.duration_ms = Some(duration_ms);
            }
        }
    }

    fn snapshot(
        &self,
        cors: &GatewayCorsConfig,
        history: Value,
        rate_limit: Option<&GatewayRateLimit>,
        redirect_rule_count: usize,
    ) -> Value {
        let counters = self
            .counters
            .lock()
            .map(|counters| counters.clone())
            .unwrap_or_default();
        let recent_decisions = self
            .recent_decisions
            .lock()
            .map(|entries| {
                entries
                    .iter()
                    .map(GatewayDecisionLogEntry::as_json)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let rate_limit = rate_limit
            .map(|rate_limit| {
                json!({
                    "activeBuckets": rate_limit.active_bucket_count(),
                    "enabled": true,
                    "maxRequests": rate_limit.config.max_requests,
                    "windowSeconds": rate_limit.config.window_seconds,
                })
            })
            .unwrap_or_else(|| {
                json!({
                    "activeBuckets": 0,
                    "enabled": false,
                })
            });

        json!({
            "config": {
                "cors": {
                    "allowedHeaders": &cors.allowed_headers,
                    "maxAgeSeconds": cors.max_age_seconds,
                    "methods": &cors.methods,
                    "origin": &cors.origin,
                },
                "rateLimit": rate_limit.clone(),
                "redirectRules": {
                    "count": redirect_rule_count,
                },
            },
            "history": history,
            "rateLimit": rate_limit,
            "recentDecisions": recent_decisions,
            "requests": {
                "continued": counters.continued,
                "preflight": counters.preflight,
                "rateLimited": counters.rate_limited,
                "redirected": counters.redirected,
                "total": counters.total,
            },
        })
    }
}

fn split_path_query(uri: &str) -> (&str, Option<&str>) {
    match uri.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (uri, None),
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn now_millis() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
        .min(i64::MAX as u128);
    millis as i64
}

fn optional_u16(value: Option<u16>) -> StateValue {
    value
        .map(|value| StateValue::Integer(i64::from(value)))
        .unwrap_or(StateValue::Null)
}

fn optional_u64(value: Option<u64>) -> StateValue {
    value
        .map(|value| StateValue::Integer(value.min(i64::MAX as u64) as i64))
        .unwrap_or(StateValue::Null)
}

/// Minimal gateway middleware — CORS plus deterministic redirect rules.
pub struct GatewayExtension {
    cors: GatewayCorsConfig,
    diagnostics: GatewayDiagnostics,
    history_store: Option<GatewayHistoryStore>,
    prefix: String,
    invocations: Arc<AtomicU32>,
    rate_limit: Option<GatewayRateLimit>,
    redirect_rules: Vec<GatewayRedirectRule>,
}

impl GatewayExtension {
    pub fn new() -> Self {
        Self::with_prefix("")
    }

    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        Self {
            cors: GatewayCorsConfig::default(),
            diagnostics: GatewayDiagnostics::default(),
            history_store: None,
            prefix: prefix.into(),
            invocations: Arc::new(AtomicU32::new(0)),
            rate_limit: None,
            redirect_rules: vec![],
        }
    }

    pub fn with_cors(mut self, cors: GatewayCorsConfig) -> Self {
        self.cors = cors;
        self
    }

    pub fn with_rate_limit(mut self, config: GatewayRateLimitConfig) -> Self {
        self.rate_limit = Some(GatewayRateLimit::new(config));
        self
    }

    pub fn with_redirect_rules(mut self, redirect_rules: Vec<GatewayRedirectRule>) -> Self {
        self.redirect_rules = redirect_rules;
        self
    }

    pub fn with_history_store(
        mut self,
        sql: Arc<dyn DurableSqlProvider>,
        namespace: impl Into<String>,
    ) -> Self {
        self.history_store = Some(GatewayHistoryStore::new(sql, namespace));
        self
    }

    /// Factory for explicit bin registration (story 06.01 pattern).
    pub fn middleware() -> Arc<dyn Middleware> {
        Arc::new(Self::new())
    }

    pub fn invocation_count(&self) -> u32 {
        self.invocations.load(Ordering::SeqCst)
    }

    pub fn persistent_decision_count(&self) -> Result<u64, CoreError> {
        self.history_store
            .as_ref()
            .map(GatewayHistoryStore::decision_count)
            .unwrap_or(Ok(0))
    }

    fn has_test_header(req: &SerializedRequest) -> bool {
        req.headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(GATEWAY_TEST_HEADER))
    }

    fn request_header<'a>(req: &'a SerializedRequest, name: &str) -> Option<&'a str> {
        req.headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn is_cors_preflight(req: &SerializedRequest) -> bool {
        req.method.eq_ignore_ascii_case("OPTIONS") && Self::request_header(req, "origin").is_some()
    }

    fn cors_headers(&self, req: Option<&SerializedRequest>) -> Vec<(String, String)> {
        let mut headers = vec![
            (
                "access-control-allow-origin".into(),
                self.cors.origin.clone(),
            ),
            (
                "access-control-allow-methods".into(),
                self.cors.methods.join(", "),
            ),
        ];
        if let Some(req) = req {
            let allowed_headers = Self::request_header(req, "access-control-request-headers")
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| self.cors.allowed_headers.join(", "));
            if !allowed_headers.is_empty() {
                headers.push(("access-control-allow-headers".into(), allowed_headers));
            }
            headers.push((
                "access-control-max-age".into(),
                self.cors.max_age_seconds.to_string(),
            ));
        }
        headers
    }

    fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
        if let Some((_, existing)) = headers
            .iter_mut()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        {
            *existing = value;
        } else {
            headers.push((name.to_string(), value));
        }
    }

    fn redirect_response(&self, req: &SerializedRequest) -> Option<SerializedResponse> {
        let (rule, location) = self
            .redirect_rules
            .iter()
            .find_map(|rule| rule.location_for(&req.uri).map(|location| (rule, location)))?;
        Some(SerializedResponse {
            body: None,
            headers: vec![("location".into(), location)],
            status: rule.status,
        })
    }

    fn rate_limit_response(&self, req: &SerializedRequest) -> Result<Option<SerializedResponse>> {
        let Some(rate_limit) = &self.rate_limit else {
            return Ok(None);
        };
        let decision = Self::rate_limit_decision(rate_limit, req)?;
        if decision.allowed {
            return Ok(None);
        }

        let mut headers = self.cors_headers(None);
        headers.push((
            RATE_LIMIT_LIMIT_HEADER.into(),
            rate_limit.config.max_requests.to_string(),
        ));
        headers.push((RATE_LIMIT_REMAINING_HEADER.into(), "0".into()));
        headers.push((
            RETRY_AFTER_HEADER.into(),
            decision.retry_after_seconds.to_string(),
        ));
        Ok(Some(SerializedResponse {
            body: None,
            headers,
            status: 429,
        }))
    }

    fn rate_limit_decision(
        rate_limit: &GatewayRateLimit,
        req: &SerializedRequest,
    ) -> Result<RateLimitDecision> {
        let key = Self::rate_limit_key(&rate_limit.config, req);
        let now = Instant::now();
        let mut buckets = rate_limit
            .buckets
            .lock()
            .map_err(|_| anyhow!("gateway rate limit state poisoned"))?;
        let capacity = f64::from(rate_limit.config.max_requests);
        let refill_rate = capacity / rate_limit.config.window_seconds as f64;
        let bucket = buckets.entry(key).or_insert_with(|| RateLimitBucket {
            last_refill: now,
            tokens: capacity,
        });

        let elapsed_seconds = now.duration_since(bucket.last_refill).as_secs_f64();
        if elapsed_seconds > 0.0 {
            bucket.tokens = capacity.min(bucket.tokens + elapsed_seconds * refill_rate);
            bucket.last_refill = now;
        }

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return Ok(RateLimitDecision {
                allowed: true,
                remaining: bucket.tokens.floor() as u32,
                retry_after_seconds: 0,
            });
        }

        let retry_after_seconds = ((1.0 - bucket.tokens) / refill_rate).ceil().max(1.0) as u64;
        Ok(RateLimitDecision {
            allowed: false,
            remaining: 0,
            retry_after_seconds,
        })
    }

    fn rate_limit_key(config: &GatewayRateLimitConfig, req: &SerializedRequest) -> String {
        if let Some(header) = &config.key_header {
            let value = Self::request_header(req, header)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("unknown");
            return format!("header:{header}:{value}");
        }

        let forwarded_for = Self::request_header(req, "x-forwarded-for")
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let real_ip = Self::request_header(req, "x-real-ip")
            .map(str::trim)
            .filter(|value| !value.is_empty());
        format!("ip:{}", forwarded_for.or(real_ip).unwrap_or("unknown"))
    }

    fn diagnostics_client(req: &SerializedRequest) -> String {
        let forwarded_for = Self::request_header(req, "x-forwarded-for")
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let real_ip = Self::request_header(req, "x-real-ip")
            .map(str::trim)
            .filter(|value| !value.is_empty());
        forwarded_for.or(real_ip).unwrap_or("unknown").to_string()
    }

    fn record_decision(
        &self,
        req: &SerializedRequest,
        decision: &'static str,
        status: Option<u16>,
        rate_limited: bool,
        duration_ms: Option<u64>,
    ) {
        let (path, _) = split_path_query(&req.uri);
        let entry = GatewayDecisionLogEntry {
            client: Self::diagnostics_client(req),
            decision,
            duration_ms,
            method: req.method.clone(),
            path: path.to_string(),
            rate_limited,
            request_id: req.request_id.clone(),
            status,
        };
        self.diagnostics.record(entry.clone());
        if let Some(history_store) = &self.history_store {
            if let Err(error) = history_store.record(&entry) {
                trace!(
                    error_code = %error.code,
                    extension = self.name(),
                    "gateway persistent history write failed"
                );
            }
        }
    }

    fn persistent_history_diagnostics(&self) -> Value {
        let Some(history_store) = &self.history_store else {
            return json!({
                "persistent": {
                    "enabled": false,
                },
            });
        };

        match history_store.decision_count() {
            Ok(decisions) => json!({
                "persistent": {
                    "decisions": decisions,
                    "enabled": true,
                },
            }),
            Err(error) => json!({
                "persistent": {
                    "enabled": true,
                    "errorCode": error.code,
                },
            }),
        }
    }
}

impl Default for GatewayExtension {
    fn default() -> Self {
        Self::new()
    }
}

impl Extension for GatewayExtension {
    fn capabilities(&self) -> Vec<ExtensionCapability> {
        vec![
            ExtensionCapability::Middleware,
            ExtensionCapability::RequestHook,
            ExtensionCapability::ResponseHook,
        ]
    }

    fn name(&self) -> &'static str {
        "gateway"
    }

    fn priority(&self) -> i32 {
        0
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        trace!(extension = self.name(), "gateway extension initialized");
        Ok(())
    }

    fn diagnostics(&self) -> Option<Value> {
        Some(self.diagnostics.snapshot(
            &self.cors,
            self.persistent_history_diagnostics(),
            self.rate_limit.as_ref(),
            self.redirect_rules.len(),
        ))
    }
}

impl Middleware for GatewayExtension {
    fn on_request(
        &self,
        req: &mut SerializedRequest,
        ctx: &RequestContext,
    ) -> Result<Option<SerializedResponse>> {
        if Self::has_test_header(req) {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            trace!(
                extension = self.name(),
                uri = %req.uri,
                prefix = %self.prefix,
                "gateway on_request (test header)"
            );
        }
        if Self::is_cors_preflight(req) {
            self.record_decision(
                req,
                "preflight",
                Some(204),
                false,
                Some(elapsed_ms(ctx.start)),
            );
            return Ok(Some(SerializedResponse {
                body: None,
                headers: self.cors_headers(Some(req)),
                status: 204,
            }));
        }
        if let Some(response) = self.rate_limit_response(req)? {
            self.record_decision(
                req,
                "rate_limited",
                Some(response.status),
                true,
                Some(elapsed_ms(ctx.start)),
            );
            return Ok(Some(response));
        }
        if let Some(response) = self.redirect_response(req) {
            self.record_decision(
                req,
                "redirect",
                Some(response.status),
                false,
                Some(elapsed_ms(ctx.start)),
            );
            return Ok(Some(response));
        }
        self.record_decision(req, "continue", None, false, None);
        Ok(None)
    }

    fn on_response(&self, res: &mut SerializedResponse, ctx: &RequestContext) {
        let duration_ms = elapsed_ms(ctx.start);
        self.diagnostics
            .complete_response(&ctx.request_id, res.status, duration_ms);
        if let Some(history_store) = &self.history_store {
            if let Err(error) =
                history_store.complete_response(&ctx.request_id, res.status, duration_ms)
            {
                trace!(
                    error_code = %error.code,
                    extension = self.name(),
                    "gateway persistent history update failed"
                );
            }
        }
        for (name, value) in self.cors_headers(None) {
            Self::set_header(&mut res.headers, &name, value);
        }
    }
}
