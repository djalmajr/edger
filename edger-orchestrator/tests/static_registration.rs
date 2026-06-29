//! Static extension registration tests (story 06.01).

use std::sync::Arc;

use anyhow::Result;
use edger_core::{
    Extension, ExtensionContext, Middleware, RequestContext, SerializedRequest, SerializedResponse,
};
use edger_orchestrator::collect_extensions;

struct MockExt {
    name: &'static str,
    priority: i32,
}

impl Extension for MockExt {
    fn name(&self) -> &'static str {
        self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn on_init(&self, _ctx: &mut ExtensionContext) -> Result<()> {
        Ok(())
    }
}

impl Middleware for MockExt {
    fn on_request(
        &self,
        _req: &mut SerializedRequest,
        _ctx: &RequestContext,
    ) -> Result<Option<SerializedResponse>> {
        Ok(None)
    }
}

#[test]
fn collect_extensions_empty_registry() {
    let registry = collect_extensions(vec![]).unwrap();
    assert!(registry.is_empty());
}

#[test]
fn explicit_registration_appears_in_registry() {
    let registry = collect_extensions(vec![
        Arc::new(MockExt {
            name: "ext-a",
            priority: 0,
        }),
        Arc::new(MockExt {
            name: "ext-b",
            priority: -5,
        }),
    ])
    .unwrap();

    assert_eq!(registry.len(), 2);
    assert_eq!(registry.middlewares()[0].name(), "ext-b");
    assert_eq!(registry.middlewares()[1].name(), "ext-a");
}

#[test]
fn duplicate_extension_name_fails() {
    match collect_extensions(vec![
        Arc::new(MockExt {
            name: "dup",
            priority: 0,
        }),
        Arc::new(MockExt {
            name: "dup",
            priority: 1,
        }),
    ]) {
        Err(err) => assert_eq!(err.code, "COLLISION"),
        Ok(_) => panic!("expected collision error"),
    }
}
