//! Extension registry — ordered middleware storage (story 05.05).

use std::sync::Arc;

use edger_core::{CoreError, Middleware};

/// Registry of middleware extensions sorted by `priority()` (lower runs first).
#[derive(Clone, Default)]
pub struct ExtensionRegistry {
    middlewares: Arc<Vec<Arc<dyn Middleware>>>,
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, middleware: Arc<dyn Middleware>) -> Result<(), CoreError> {
        let entries = Arc::make_mut(&mut self.middlewares);
        if entries
            .iter()
            .any(|existing| existing.name() == middleware.name())
        {
            return Err(CoreError::new(
                "COLLISION",
                format!("extension already registered: {}", middleware.name()),
            ));
        }
        entries.push(middleware);
        entries.sort_by_key(|m| m.priority());
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.middlewares.len()
    }

    pub fn is_empty(&self) -> bool {
        self.middlewares.is_empty()
    }

    pub fn middlewares(&self) -> &[Arc<dyn Middleware>] {
        &self.middlewares
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use edger_core::{
        Extension, ExtensionContext, Middleware, RequestContext, SerializedRequest,
        SerializedResponse,
    };

    struct NamedMiddleware {
        name: &'static str,
        priority: i32,
    }

    impl Extension for NamedMiddleware {
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

    impl Middleware for NamedMiddleware {
        fn on_request(
            &self,
            _req: &mut SerializedRequest,
            _ctx: &RequestContext,
        ) -> Result<Option<SerializedResponse>> {
            Ok(None)
        }
    }

    #[test]
    fn rejects_duplicate_names() {
        let mut registry = ExtensionRegistry::new();
        registry
            .register(Arc::new(NamedMiddleware {
                name: "dup",
                priority: 0,
            }))
            .unwrap();
        let err = registry
            .register(Arc::new(NamedMiddleware {
                name: "dup",
                priority: 1,
            }))
            .unwrap_err();
        assert_eq!(err.code, "COLLISION");
    }

    #[test]
    fn sorts_by_priority() {
        let mut registry = ExtensionRegistry::new();
        registry
            .register(Arc::new(NamedMiddleware {
                name: "late",
                priority: 10,
            }))
            .unwrap();
        registry
            .register(Arc::new(NamedMiddleware {
                name: "early",
                priority: -10,
            }))
            .unwrap();
        assert_eq!(registry.middlewares()[0].name(), "early");
        assert_eq!(registry.middlewares()[1].name(), "late");
    }
}