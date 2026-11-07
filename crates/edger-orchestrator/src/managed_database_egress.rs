//! Dynamic private database egress (EDG-54 15b).
//!
//! The host object that connects the provisioning service
//! ([`SqldProvisionService`]) to the REAL transport: the single shared
//! [`EgressGuard`]. One object binds ONE trusted worker identity (the
//! `WorkerRef`: worker, version, RBAC namespace) and ONE fixed, validated
//! plain-HTTP SQLD data origin; both are host data fixed at construction,
//! never read from a request, an env, a manifest or a guest frame.
//!
//! The authority is DYNAMIC: nothing is cached and no token is copied into
//! this object. Every call the guard admits asks the service, in this
//! order, (1) for the binding's metadata (it must be `Ready`; `Intent`,
//! `Proven`, `Conflict`, absent and errors all deny — this code never
//! creates or ensures anything), then (2) builds the grant of the returned
//! SQLD namespace for the fixed origin and the `SQLD_TOKEN` credential name
//! with the service's OWN per-owner resolver
//! ([`SqldProvisionService::resolver_for`]), which verifies the stored
//! token's state, signature and claims and renews it through the vault's
//! compare-and-set. The guard imposes `x-namespace-bin` and the bearer
//! token and removes whatever the app sent.
//!
//! Two sockets of the same authority:
//!
//! * Deno: [`ManagedDatabaseEgress::listener_sink`] is the
//!   [`EgressListenerSink`] of the private managed socket (the bootstrap
//!   forwards ONLY the native database origin to it). The sink serves the
//!   socket only when the context identity equals the bound one; the DB
//!   origin goes ONLY to this authority (no fallback on any failure) and
//!   every other destination goes to the general path only when the
//!   context carries an explicit `settings: Some`.
//! * WASM: [`ManagedDatabaseEgress::wasm_transport`] is the
//!   [`WasmHttpTransport`] of the private service field, bound to the same
//!   identity and the same guard; the dynamic isolate builder lives in
//!   `egress_wasm_bridge`.
//!
//! This is NOT the product wiring: there is no factory, CLI, API or spawn
//! configuration here. A future factory builds one object per `WorkerRef`
//! and hands it to the Deno setter or the WASM builder.

use std::sync::Arc;

use async_trait::async_trait;
use edger_core::{AppSecretResolver, EgressSettings, WorkerRef};
use edger_isolation::wasm::{WasmHttpError, WasmHttpRequest, WasmHttpResponse, WasmHttpTransport};
use edger_isolation::{EgressListenerContext, EgressListenerSink, EgressServeFuture};

use crate::egress_database::{EgressDatabaseGrant, EgressDatabaseGrantError};
use crate::egress_guard::{
    DatabaseAuthority, DatabaseAuthorityError, DatabaseCredentials, EgressGuard,
    EgressWorkerContext,
};
use crate::sqld_provision::{SqldOwner, SqldProvisionService, SQLD_TOKEN_SECRET};
use crate::sqld_registry::SqldRegistryState;

/// Why a managed database egress cannot be built. Static classes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedDatabaseEgressError {
    /// The trusted worker identity is empty.
    InvalidIdentity,
    /// The data origin is not a valid private SQLD origin (plain HTTP, a
    /// host, a fixed port that is not the admin one, no userinfo, query,
    /// fragment or path).
    InvalidOrigin,
}

impl std::fmt::Display for ManagedDatabaseEgressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIdentity => write!(f, "invalid managed database identity"),
            Self::InvalidOrigin => write!(f, "invalid managed database origin"),
        }
    }
}

impl std::error::Error for ManagedDatabaseEgressError {}

/// The bound state. Deliberately without `Debug`: it holds the service and
/// the guard, and the identity is policy material.
struct Inner {
    service: Arc<SqldProvisionService>,
    guard: Arc<EgressGuard>,
    owner: SqldOwner,
    worker: String,
    version: String,
    namespace: Option<String>,
    origin: url::Url,
}

impl Inner {
    fn matches(&self, worker: &str, version: &str, namespace: &Option<String>) -> bool {
        self.worker == worker && self.version == version && &self.namespace == namespace
    }
}

#[async_trait]
impl DatabaseAuthority for Inner {
    fn permits(&self, ctx: &EgressWorkerContext, request_url: &str) -> bool {
        if !self.matches(&ctx.worker, &ctx.version, &ctx.namespace) {
            return false;
        }
        let Ok(request) = url::Url::parse(request_url) else {
            return false;
        };
        request.scheme() == "http"
            && request.username().is_empty()
            && request.password().is_none()
            && request.fragment().is_none()
            && request.origin() == self.origin.origin()
    }

    fn origin(&self) -> &url::Url {
        &self.origin
    }

    async fn credentials(
        &self,
        ctx: &EgressWorkerContext,
    ) -> Result<DatabaseCredentials, DatabaseAuthorityError> {
        if !self.matches(&ctx.worker, &ctx.version, &ctx.namespace) {
            return Err(DatabaseAuthorityError::Denied);
        }
        // (1) The binding must be Ready: Intent/Proven/Conflict/absent and
        //     any lookup error deny. Nothing is created here.
        let metadata = self
            .service
            .metadata(&self.owner)
            .await
            .map_err(|_| DatabaseAuthorityError::Unavailable)?;
        let Some(metadata) = metadata else {
            return Err(DatabaseAuthorityError::Denied);
        };
        if metadata.state != SqldRegistryState::Ready {
            return Err(DatabaseAuthorityError::Denied);
        }
        // (2) The grant of the returned namespace over the fixed origin,
        //     with the service's own per-owner resolver (state, signature,
        //     claims, expiry and CAS renewal live there).
        let resolver: Arc<dyn AppSecretResolver> = self.service.resolver_for(self.owner.clone());
        let grant = EgressDatabaseGrant::new(
            ctx,
            self.origin.as_str(),
            &metadata.sql_namespace,
            SQLD_TOKEN_SECRET,
            Some(resolver),
        )
        .map_err(|_| DatabaseAuthorityError::Unavailable)?;
        let authorization = grant
            .authorization(ctx)
            .await
            .map_err(|error| match error {
                EgressDatabaseGrantError::Denied => DatabaseAuthorityError::Denied,
                _ => DatabaseAuthorityError::Unavailable,
            })?;
        Ok(DatabaseCredentials {
            namespace: grant.namespace_header(),
            authorization,
        })
    }
}

/// The host-side managed database authority of ONE worker identity. See the
/// module docs. Cheap to clone (shared state); deliberately without
/// `Debug`.
#[derive(Clone)]
pub struct ManagedDatabaseEgress {
    inner: Arc<Inner>,
}

impl ManagedDatabaseEgress {
    /// Bind the service, the SINGLE shared guard, the trusted worker
    /// identity and the fixed data origin. Synchronous and free of I/O: no
    /// SQLite, no crypto, no network, no token. The origin is validated
    /// with the grant's own SQLD origin rules (plain HTTP, a host, a fixed
    /// port other than the admin 9090, no userinfo/query/fragment, path
    /// `/`). The service's own data origin is not readable from here: the
    /// operator wires the same configured value to both.
    pub fn new(
        service: Arc<SqldProvisionService>,
        guard: Arc<EgressGuard>,
        worker: &WorkerRef,
        data_origin: &str,
    ) -> Result<Self, ManagedDatabaseEgressError> {
        if worker.name.is_empty() {
            return Err(ManagedDatabaseEgressError::InvalidIdentity);
        }
        // Origin validation reuses the grant's validator (one rule set);
        // the probe grant is dropped at once and never used.
        let probe_owner = EgressWorkerContext {
            worker: worker.name.clone(),
            version: worker.version.clone(),
            namespace: worker.namespace.clone(),
            process_id: String::new(),
        };
        let probe = EgressDatabaseGrant::new(
            &probe_owner,
            data_origin,
            "origin-probe",
            SQLD_TOKEN_SECRET,
            None,
        )
        .map_err(|_| ManagedDatabaseEgressError::InvalidOrigin)?;
        Ok(Self {
            inner: Arc::new(Inner {
                service,
                guard,
                owner: SqldOwner::new(worker.name.clone(), worker.namespace.clone()),
                worker: worker.name.clone(),
                version: worker.version.clone(),
                namespace: worker.namespace.clone(),
                origin: probe.origin().clone(),
            }),
        })
    }

    /// The single shared guard this authority runs through.
    pub fn guard(&self) -> &Arc<EgressGuard> {
        &self.inner.guard
    }

    /// The normalized fixed database origin (operator metadata, never a
    /// secret).
    pub fn origin(&self) -> &url::Url {
        &self.inner.origin
    }

    /// Whether `worker` is EXACTLY the bound identity (name, version and
    /// RBAC namespace).
    pub fn binds(&self, worker: &WorkerRef) -> bool {
        self.inner
            .matches(&worker.name, &worker.version, &worker.namespace)
    }

    /// The Deno private-socket sink: serves a socket only when the context
    /// identity equals the bound one (else the listener is dropped, which
    /// closes it). `settings: Some` in the context is the ONLY thing that
    /// opens the general path for non-database destinations; `limits`
    /// carry no authority.
    pub fn listener_sink(&self) -> Arc<dyn EgressListenerSink> {
        Arc::new(ManagedSink {
            inner: Arc::clone(&self.inner),
        })
    }

    /// The WASM private transport for the bound identity with the given
    /// limits profile (limits only, never authority). `None` when `worker`
    /// is not the bound identity.
    pub fn wasm_transport(
        &self,
        worker: &WorkerRef,
        limits: EgressSettings,
    ) -> Option<Arc<dyn WasmHttpTransport>> {
        if !self.binds(worker) {
            return None;
        }
        Some(Arc::new(ManagedWasmTransport {
            inner: Arc::clone(&self.inner),
            ctx: Arc::new(EgressWorkerContext {
                worker: worker.name.clone(),
                version: worker.version.clone(),
                namespace: worker.namespace.clone(),
                process_id: String::new(),
            }),
            limits: Arc::new(limits),
        }))
    }
}

struct ManagedSink {
    inner: Arc<Inner>,
}

impl EgressListenerSink for ManagedSink {
    fn serve(
        &self,
        listener: tokio::net::UnixListener,
        context: EgressListenerContext,
    ) -> EgressServeFuture {
        if !self
            .inner
            .matches(&context.worker, &context.version, &context.namespace)
        {
            // A context that is not the bound identity gets no service:
            // dropping the listener closes the socket.
            tracing::warn!("managed database socket refused: identity is not the bound one");
            drop(listener);
            return Box::pin(async {});
        }
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let EgressListenerContext {
                worker,
                version,
                namespace,
                process_id,
                settings,
                limits,
            } = context;
            let guard = Arc::clone(&inner.guard);
            let authority: Arc<dyn DatabaseAuthority> = inner;
            guard
                .serve_managed(
                    listener,
                    EgressWorkerContext {
                        worker,
                        version,
                        namespace,
                        process_id,
                    },
                    settings,
                    limits,
                    authority,
                )
                .await;
        })
    }
}

struct ManagedWasmTransport {
    inner: Arc<Inner>,
    ctx: Arc<EgressWorkerContext>,
    limits: Arc<EgressSettings>,
}

#[async_trait]
impl WasmHttpTransport for ManagedWasmTransport {
    async fn fetch(
        &self,
        request: WasmHttpRequest,
        response_body_limit: usize,
    ) -> Result<WasmHttpResponse, WasmHttpError> {
        let authority: Arc<dyn DatabaseAuthority> = self.inner.clone();
        self.inner
            .guard
            .fetch_private(
                request,
                self.ctx.clone(),
                self.limits.clone(),
                authority,
                response_body_limit,
            )
            .await
    }
}
