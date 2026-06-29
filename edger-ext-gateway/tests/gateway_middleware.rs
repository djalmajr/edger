//! Gateway middleware tests (story 06.03).

use std::sync::Arc;

use edger_core::{Middleware, RequestContext, SerializedRequest};
use edger_ext_gateway::GatewayExtension;

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
