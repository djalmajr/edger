//! Mock isolate backend for tests and WorkerPool integration (Epic 04).

use async_trait::async_trait;
use bytes::Bytes;

use edger_core::{Isolate, IsolationError, SerializedRequest, SerializedResponse, WorkerConfig};

/// Configurable mock isolate — no V8/Wasm engine required.
pub struct MockIsolate {
    spa_html: String,
    terminate_calls: u32,
    idle_calls: u32,
    fail_on_terminate: bool,
}

impl MockIsolate {
    pub fn new() -> Self {
        Self {
            spa_html: "<html><body>mock-spa</body></html>".into(),
            terminate_calls: 0,
            idle_calls: 0,
            fail_on_terminate: false,
        }
    }

    pub fn with_spa_html(mut self, html: impl Into<String>) -> Self {
        self.spa_html = html.into();
        self
    }

    pub fn with_fail_on_terminate(mut self, fail: bool) -> Self {
        self.fail_on_terminate = fail;
        self
    }

    pub fn terminate_count(&self) -> u32 {
        self.terminate_calls
    }

    pub fn idle_count(&self) -> u32 {
        self.idle_calls
    }

    fn echo_response(prefix: &str, req: &SerializedRequest, status: u16) -> SerializedResponse {
        SerializedResponse {
            status,
            headers: vec![],
            body: Some(Bytes::from(format!("{prefix}:{} {}", req.method, req.uri))),
        }
    }

    fn inject_base_href(html: &str, base_href: &str) -> String {
        if html.contains("<head>") {
            html.replace("<head>", &format!(r#"<head><base href="{base_href}">"#))
        } else {
            format!(r#"<base href="{base_href}">{html}"#)
        }
    }
}

impl Default for MockIsolate {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Isolate for MockIsolate {
    async fn execute_fetch(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(Self::echo_response("fetch", &req, 200))
    }

    async fn execute_routes(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(Self::echo_response("routes", &req, 200))
    }

    async fn serve_static_spa(
        &mut self,
        path: &str,
        base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let mut body = self.spa_html.clone();
        if let Some(base) = base_href {
            body = Self::inject_base_href(&body, base);
        }
        Ok(SerializedResponse {
            status: 200,
            headers: vec![("content-type".into(), "text/html".into())],
            body: Some(Bytes::from(format!("{body}<!-- path={path} -->"))),
        })
    }

    async fn execute_wasm(
        &mut self,
        req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        let mut res = Self::echo_response("wasm", &req, 200);
        res.headers.push(("x-mock-wasm".into(), "1".into()));
        Ok(res)
    }

    async fn notify_idle(&mut self) -> Result<(), IsolationError> {
        self.idle_calls += 1;
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), IsolationError> {
        self.terminate_calls += 1;
        if self.fail_on_terminate && self.terminate_calls == 1 {
            return Err(IsolationError::new("TERMINATE_FAIL", "injected failure"));
        }
        Ok(())
    }
}
