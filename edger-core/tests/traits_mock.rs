//! Trait mock compile/smoke tests (story 02.04).

use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use async_trait::async_trait;
use edger_core::{
    create_worker_ref, AuthProvider, Extension, ExtensionContext, Isolate, IsolationError,
    Middleware, RequestContext, SerializedRequest, SerializedResponse, WorkerConfig, WorkerHandler,
    WorkerManifest, WorkerRef,
};

struct NoopMiddleware {
    hits: AtomicUsize,
}

impl Extension for NoopMiddleware {
    fn name(&self) -> &'static str {
        "noop"
    }

    fn priority(&self) -> i32 {
        -10
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        Ok(())
    }
}

impl Middleware for NoopMiddleware {
    fn on_request(
        &self,
        _req: &mut SerializedRequest,
        _ctx: &RequestContext,
    ) -> Result<Option<SerializedResponse>> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
}

struct ShortCircuitMiddleware;

impl Extension for ShortCircuitMiddleware {
    fn name(&self) -> &'static str {
        "short"
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        Ok(())
    }
}

impl Middleware for ShortCircuitMiddleware {
    fn on_request(
        &self,
        _req: &mut SerializedRequest,
        _ctx: &RequestContext,
    ) -> Result<Option<SerializedResponse>> {
        Ok(Some(SerializedResponse {
            status: 418,
            headers: vec![],
            body: None,
        }))
    }
}

struct MockAuth;

impl Extension for MockAuth {
    fn name(&self) -> &'static str {
        "mock-auth"
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        Ok(())
    }
}

impl AuthProvider for MockAuth {
    fn authenticate(
        &self,
        headers: &[(String, String)],
    ) -> Result<Option<edger_core::ApiKeyPrincipal>> {
        if headers
            .iter()
            .any(|(k, _): &(String, String)| k.eq_ignore_ascii_case("x-api-key"))
        {
            Ok(Some(edger_core::root_principal()))
        } else {
            Ok(None)
        }
    }
}

struct MockHandler;

#[async_trait]
impl WorkerHandler for MockHandler {
    async fn handle(
        &self,
        _req: SerializedRequest,
        worker: &WorkerRef,
    ) -> Result<SerializedResponse> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: Some(bytes::Bytes::from(worker.name.clone())),
        })
    }
}

struct MockIsolate;

#[async_trait]
impl Isolate for MockIsolate {
    async fn execute_fetch(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: None,
        })
    }

    async fn execute_routes(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new("NOT_IMPL", "routes"))
    }

    async fn serve_static_spa(
        &mut self,
        _path: &str,
        _base_href: Option<&str>,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Ok(SerializedResponse {
            status: 200,
            headers: vec![],
            body: None,
        })
    }

    async fn execute_wasm(
        &mut self,
        _req: SerializedRequest,
        _config: &WorkerConfig,
    ) -> Result<SerializedResponse, IsolationError> {
        Err(IsolationError::new("NOT_IMPL", "wasm"))
    }
}

#[test]
fn middleware_on_request_returns_none_to_continue() {
    let mw = NoopMiddleware {
        hits: AtomicUsize::new(0),
    };
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "r".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("r");
    let out = Middleware::on_request(&mw, &mut req, &ctx).unwrap();
    assert!(out.is_none());
    assert_eq!(mw.hits.load(Ordering::SeqCst), 1);
}

#[test]
fn middleware_short_circuit_returns_some() {
    let mw = ShortCircuitMiddleware;
    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "r".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("r");
    let out = Middleware::on_request(&mw, &mut req, &ctx)
        .unwrap()
        .unwrap();
    assert_eq!(out.status, 418);
}

#[test]
fn auth_provider_mock_compiles() {
    let auth = MockAuth;
    let principal = auth
        .authenticate(&[("X-API-Key".into(), "secret".into())])
        .unwrap()
        .expect("principal");
    assert!(principal.is_root);
    assert!(auth.can_access_namespace(&principal, "@acme"));
}

#[tokio::test]
async fn worker_handler_and_isolate_mocks_compile() {
    let manifest = WorkerManifest {
        name: "hello".into(),
        ..Default::default()
    };
    let worker = create_worker_ref(std::path::PathBuf::from("/w"), manifest).unwrap();
    let handler = MockHandler;
    let req = SerializedRequest {
        method: "GET".into(),
        uri: "/".into(),
        headers: vec![],
        body: None,
        request_id: "r".into(),
        base_href: None,
    };
    let res = handler.handle(req.clone(), &worker).await.unwrap();
    assert_eq!(res.body.unwrap().as_ref(), b"hello");

    let mut isolate = MockIsolate;
    let config = worker.config.clone();
    let out = isolate.execute_fetch(req, &config).await.unwrap();
    assert_eq!(out.status, 200);
}
