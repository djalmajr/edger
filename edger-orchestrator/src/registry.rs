//! Extension registry — ordered middleware storage (story 05.05).

use std::sync::Arc;

use edger_core::{AuthProvider, CoreError, Middleware};

/// Registry of middleware extensions sorted by `priority()` (lower runs first).
#[derive(Clone, Default)]
pub struct ExtensionRegistry {
    middlewares: Arc<Vec<Arc<dyn Middleware>>>,
    auth_provider: Arc<Option<Arc<dyn AuthProvider>>>,
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

    pub fn register_auth_provider(
        &mut self,
        provider: Arc<dyn AuthProvider>,
    ) -> Result<(), CoreError> {
        let slot = Arc::make_mut(&mut self.auth_provider);
        if slot.is_some() {
            return Err(CoreError::new(
                "COLLISION",
                "auth provider already registered".to_string(),
            ));
        }
        *slot = Some(provider);
        Ok(())
    }

    pub fn auth_provider(&self) -> Option<Arc<dyn AuthProvider>> {
        (*self.auth_provider).clone()
    }

    /// Build a registry from an explicit extension list (story 06.01 — chosen pattern).
    ///
    /// The `edger` binary is the composition root: each `edger-ext-*` crate exports a
    /// constructor; the bin calls `collect_extensions()` and passes the result here.
    pub fn from_explicit<I>(middlewares: I) -> Result<Self, CoreError>
    where
        I: IntoIterator<Item = Arc<dyn Middleware>>,
    {
        let mut registry = Self::new();
        for middleware in middlewares {
            registry.register(middleware)?;
        }
        Ok(registry)
    }
}

/// Composition helper — explicit static registration (no inventory/linkme in v1).
pub fn collect_extensions(
    middlewares: Vec<Arc<dyn Middleware>>,
) -> Result<ExtensionRegistry, CoreError> {
    ExtensionRegistry::from_explicit(middlewares)
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
