//! Gateway extension wiring tests (story 06.03).

use std::sync::Arc;

use edger_core::{RequestContext, SerializedRequest};
use edger_ext_gateway::GatewayExtension;
use edger_orchestrator::{collect_extensions, run_on_request};

#[test]
fn gateway_registered_and_invoked_via_hooks() {
    let gateway = GatewayExtension::new();
    let gateway_arc: Arc<dyn edger_core::Middleware> = Arc::new(gateway);
    let count_probe = {
        let g = GatewayExtension::new();
        Arc::new(g)
    };
    let registry = collect_extensions(vec![GatewayExtension::middleware()]).unwrap();

    let mut req = SerializedRequest {
        method: "GET".into(),
        uri: "/demo".into(),
        headers: vec![("x-gateway-test".into(), "1".into())],
        body: None,
        request_id: "gw-int".into(),
        base_href: None,
    };
    let ctx = RequestContext::new("gw-int");
    assert!(run_on_request(&registry, &mut req, &ctx).unwrap().is_none());
    assert_eq!(registry.middlewares()[0].name(), "gateway");

    let _ = gateway_arc;
    let _ = count_probe;
}
