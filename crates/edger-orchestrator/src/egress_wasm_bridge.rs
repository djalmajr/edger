//! (EDG-52 07b) The host binding of the guest `edger_http_v1.fetch` import:
//! the egress guard as a [`WasmHttpTransport`].
//!
//! The three bindings — the guard, the worker identity and the egress
//! settings — are host-trusted data fixed at construction (the factory wires
//! them, 07c); the guest can only call `fetch`, and the worker/version/
//! namespace never come from the guest frame. The policy (DNS/IP/redirect/
//! TLS/size/concurrency) runs ENTIRELY in the guard: there is no second
//! policy and no guest-visible payload in any error or log (the transport's
//! errors are the stable, payload-free classes of `WasmHttpError`).

use std::sync::Arc;

use async_trait::async_trait;
use edger_core::EgressSettings;
use edger_isolation::wasm::{WasmHttpError, WasmHttpRequest, WasmHttpResponse, WasmHttpTransport};
use edger_isolation::{warn_egress_unavailable_once, WasiConfig, WasmIsolate};

use crate::egress_database::{EgressDatabaseBridge, EgressDatabaseGrant};
use crate::egress_guard::{EgressGuard, EgressWorkerContext};
use crate::managed_database_egress::ManagedDatabaseEgress;

/// (EDG-52 07b) One host-trusted binding of the guest HTTP import to the
/// egress guard for one worker identity. Deliberately without `Debug`: the
/// guard's configuration and the worker context are policy material, and
/// requests/answers are payload.
pub struct EgressWasmBridge {
    guard: Arc<EgressGuard>,
    ctx: Arc<EgressWorkerContext>,
    settings: Arc<EgressSettings>,
}

impl EgressWasmBridge {
    /// Bind the guard, the worker identity and the egress settings (the
    /// three immutable, host-trusted inputs of every call).
    pub fn new(
        guard: Arc<EgressGuard>,
        ctx: Arc<EgressWorkerContext>,
        settings: Arc<EgressSettings>,
    ) -> Self {
        Self {
            guard,
            ctx,
            settings,
        }
    }
}

#[async_trait]
impl WasmHttpTransport for EgressWasmBridge {
    async fn fetch(
        &self,
        request: WasmHttpRequest,
        response_body_limit: usize,
    ) -> Result<WasmHttpResponse, WasmHttpError> {
        self.guard
            .fetch_wasm(
                request,
                self.ctx.clone(),
                self.settings.clone(),
                response_body_limit,
            )
            .await
    }
}

/// (EDG-52 07c) The shared WASM assembly of the `edger` factory: the
/// single product path the bin's `ExecutionKind::WasmModule` branch and
/// the orchestrator integration tests both run. The body copies the
/// bin's WASM branch verbatim — `WasiConfig` from the per-call
/// `WorkerConfig`, the `WasmIsolate` bound to the trusted worker
/// identity (the `WorkerRef` — never request/URI/headers) and the app
/// secrets binding (a declared name the vault cannot resolve leaves the
/// variable absent with ONE warn per worker/name — never a value) — and
/// adds the egress binding: the guest `edger_http_v1.fetch` import
/// reaches the guard ONLY with BOTH the per-call grant
/// (`worker_ref.config.egress` is `Some`) and a guard instance, through
/// an immutable `EgressWasmBridge` (worker/version/namespace from the
/// `WorkerRef`; `process_id` empty — the `WorkerRef` carries no process
/// identity and the WASM backend has one isolate, not per-request
/// processes). With a grant but no guard the same generic
/// `warn_egress_unavailable_once` the bin used to emit fires (no payload)
/// and no transport is bound; the runtime's own default-deny still
/// applies (the import is admitted per execution only with the binding
/// AND the per-call grant — there are no raw WASI sockets). Values are
/// NEVER resolved here: the per-execution resolution stays inside the
/// isolate, and nothing in `WorkerConfig.env`/`public_env`, the
/// `WasiConfig` `Debug`, the module cache or this assembly changes.
pub fn assemble_wasm_isolate(
    worker_ref: &edger_core::WorkerRef,
    app_secret_resolver: Option<Arc<dyn edger_core::AppSecretResolver>>,
    egress_guard: Option<Arc<EgressGuard>>,
) -> WasmIsolate {
    let isolate = WasmIsolate::new(WasiConfig::from_worker_config(&worker_ref.config));
    let isolate = match &app_secret_resolver {
        Some(resolver) => isolate.with_app_secrets(worker_ref.name.clone(), Arc::clone(resolver)),
        None if !worker_ref.config.secrets.is_empty() => {
            // Sem cofre o worker segue sem segredos: um warn por
            // nome declarado (worker/nome apenas — nunca o valor).
            for name in &worker_ref.config.secrets {
                tracing::warn!(
                    worker = %worker_ref.name,
                    secret = %name,
                    "app secret is not injected in wasm mode (the app secrets vault is unavailable): the worker will see the variable as absent"
                );
            }
            isolate
        }
        None => isolate,
    };
    match (&worker_ref.config.egress, &egress_guard) {
        // Grant + guard: the immutable, host-trusted binding (settings
        // cloned from the per-call config — the guard owns its own
        // policy, this only carries the version's grant).
        (Some(settings), Some(guard)) => {
            let ctx = EgressWorkerContext {
                worker: worker_ref.name.clone(),
                version: worker_ref.version.clone(),
                namespace: worker_ref.namespace.clone(),
                process_id: String::new(),
            };
            isolate.with_http_transport(Arc::new(EgressWasmBridge::new(
                Arc::clone(guard),
                Arc::new(ctx),
                Arc::new(settings.clone()),
            )))
        }
        // Grant without a guard: the generic warn (no payload); the
        // isolate stays HTTP-less and the runtime's default-deny holds.
        (Some(_), None) => {
            warn_egress_unavailable_once(
                &worker_ref.name,
                &worker_ref.version,
                "wasm HTTP transport is unavailable (egress guard was not bound)",
            );
            isolate
        }
        // No grant: nothing to bind, nothing to warn.
        _ => isolate,
    }
}

/// (EDG-52/54 07d3b) The limits profile of the private service binding:
/// the per-call egress settings when the version carries the general
/// grant, else the SAME fallback values core's `normalize_egress` applies
/// to the absent manifest fields (the `DEFAULT_EGRESS_*` constants — no
/// invented values). The profile is a LIMITS profile only: it is NEVER
/// written into `WorkerConfig.egress` and NEVER cached as a per-execution
/// permission (the per-execution selection stays inside the runtime).
fn egress_limits_profile(egress: &Option<EgressSettings>) -> EgressSettings {
    egress.clone().unwrap_or_else(|| EgressSettings {
        ports: edger_core::config::DEFAULT_EGRESS_PORTS.to_vec(),
        connect_timeout_ms: edger_core::config::DEFAULT_EGRESS_CONNECT_TIMEOUT_MS,
        headers_timeout_ms: edger_core::config::DEFAULT_EGRESS_HEADERS_TIMEOUT_MS,
        idle_timeout_ms: edger_core::config::DEFAULT_EGRESS_IDLE_TIMEOUT_MS,
        total_timeout_ms: edger_core::config::DEFAULT_EGRESS_TOTAL_TIMEOUT_MS,
        max_response_bytes: edger_core::config::DEFAULT_EGRESS_MAX_RESPONSE_BYTES,
        max_concurrent: edger_core::config::DEFAULT_EGRESS_MAX_CONCURRENT,
    })
}

/// (EDG-52/54 07d3b) The PRIVATE router of the isolate's GENERAL HTTP
/// field when the version carries BOTH the general egress grant and the
/// private database authority: a URL whose NORMALIZED origin
/// (scheme/host/fixed port) equals the grant's goes ONLY to the DB
/// bridge; every other URL goes ONLY to the general guard bridge. The
/// selection is owner-INDEPENDENT (a URL on the grant origin with the
/// wrong owner goes to the DB bridge and is refused THERE, without any
/// fallback) and URL-preserving (a userinfo or fragment on the same
/// origin is routed to the DB bridge and refused there; the URL is never
/// rewritten or retargeted). One call, one transport: the result —
/// success or error — is returned as-is; there is no retry and no
/// transport fallback after an error. Deliberately without `Debug`: the
/// bridges hold policy material.
struct WasmServiceRouter {
    db_origin: url::Origin,
    db: Arc<dyn WasmHttpTransport>,
    general: Arc<EgressWasmBridge>,
}

#[async_trait]
impl WasmHttpTransport for WasmServiceRouter {
    async fn fetch(
        &self,
        request: WasmHttpRequest,
        response_body_limit: usize,
    ) -> Result<WasmHttpResponse, WasmHttpError> {
        let to_db = url::Url::parse(&request.url)
            .ok()
            .is_some_and(|request_url| request_url.origin() == self.db_origin);
        if to_db {
            self.db.fetch(request, response_body_limit).await
        } else {
            self.general.fetch(request, response_body_limit).await
        }
    }
}

/// (EDG-52/54 07d3b) The shared WASM assembly with the PRIVATE database
/// authority (the 07d1/07d2b grant through the SAME shared guard):
/// delegates the base assembly FIRST (the `WasiConfig`, the app secrets
/// binding and the general egress binding are never duplicated here). A
/// `None` grant returns the base isolate. With a grant but no guard the
/// service is NOT bound (the runtime's default-deny holds; one static
/// warn, no secret). With grant AND guard: the identity comes ONLY from
/// the `WorkerRef` (worker/version/namespace; `process_id` empty — the
/// WASM backend has one isolate, not per-request processes), the SAME
/// `Arc` guard as the general binding is shared, the limits profile is
/// the per-call egress settings when present else the guard's own
/// fallback (the `egress_limits_profile` above), and the grant carries
/// the owner/origin/vault reference (the token is resolved only at use,
/// per call, inside the guard entry — never extracted or inferred here).
/// With the general grant present the SAME general field runs the
/// private router (the grant origin -> ONLY the DB bridge; every other
/// URL -> ONLY the general bridge) and the private service field
/// receives ONLY the DB bridge; the runtime selects the general field
/// per execution ONLY while the per-call config carries the grant. The
/// base 3-parameter assembly keeps its exact shape and behavior.
/// `WorkerRef`/`WorkerConfig`/`WasiConfig` and the public env gain no
/// value or authority.
pub fn assemble_wasm_isolate_with_database(
    worker_ref: &edger_core::WorkerRef,
    app_secret_resolver: Option<Arc<dyn edger_core::AppSecretResolver>>,
    egress_guard: Option<Arc<EgressGuard>>,
    database_grant: Option<Arc<EgressDatabaseGrant>>,
) -> WasmIsolate {
    // The base assembly FIRST — delegated, never duplicated (the base
    // takes the guard by `Option<Arc<...>>`, cloned here — the SAME Arc
    // instance, not a reborrow).
    let isolate = assemble_wasm_isolate(worker_ref, app_secret_resolver, egress_guard.clone());

    // No private authority: exactly the base assembly.
    let Some(grant) = database_grant.as_ref() else {
        return isolate;
    };

    // A private grant without a guard: no service binding (the
    // runtime's default-deny holds); one static warn, without any secret.
    let Some(guard) = egress_guard.as_ref() else {
        warn_egress_unavailable_once(
            &worker_ref.name,
            &worker_ref.version,
            "wasm database service transport is unavailable (egress guard was not bound)",
        );
        return isolate;
    };

    // The worker identity comes ONLY from the WorkerRef (worker/version/
    // namespace; process_id empty). The SAME Arc guard as the general
    // binding. `ctx` is cloned at every use (the DB bridge and the
    // general router branch both hold the SAME context).
    let ctx = Arc::new(EgressWorkerContext {
        worker: worker_ref.name.clone(),
        version: worker_ref.version.clone(),
        namespace: worker_ref.namespace.clone(),
        process_id: String::new(),
    });
    let settings = Arc::new(egress_limits_profile(&worker_ref.config.egress));
    let db_bridge = Arc::new(EgressDatabaseBridge::new(
        Arc::clone(guard),
        Arc::clone(&ctx),
        settings,
        Arc::clone(grant),
    ));

    match &worker_ref.config.egress {
        // General grant + guard: the base bound the general bridge; the
        // SAME general field now runs the PRIVATE router (the grant
        // origin goes ONLY to the DB bridge, every other URL ONLY to the
        // general bridge), while the private service field receives ONLY
        // the DB bridge.
        Some(settings) => {
            let general = Arc::new(EgressWasmBridge::new(
                Arc::clone(guard),
                Arc::clone(&ctx),
                Arc::new(settings.clone()),
            ));
            let router = Arc::new(WasmServiceRouter {
                db_origin: grant.origin().origin(),
                db: Arc::clone(&db_bridge) as Arc<dyn WasmHttpTransport>,
                general,
            });
            isolate
                .with_http_transport(router)
                .with_service_http_transport(db_bridge)
        }
        // DB-only (no general grant): the private service field ONLY.
        None => isolate.with_service_http_transport(db_bridge),
    }
}

/// (EDG-54 15b) The DYNAMIC private database builder: the same isolate
/// shape as [`assemble_wasm_isolate_with_database`], but the private
/// authority is the service-backed [`ManagedDatabaseEgress`] instead of a
/// static grant, so nothing about the database is decided here: every call
/// asks the service (Ready metadata + the service's own token resolver)
/// inside the guard, after admission. The base assembly (`WasiConfig`, app
/// secrets, the general binding) is delegated FIRST and unchanged; this
/// function only adds the private service transport — and, when the
/// version also declares the general egress, the same private router (the
/// database origin goes ONLY to the managed transport, every other URL
/// ONLY to the general bridge). The caller builds ONE isolate per worker
/// (the module cache and the resolvers are the base's); nothing is built
/// per request. The ONE guard is the managed object's: a different guard
/// passed alongside it leaves the private authority unbound (one static
/// warn, default-deny holds), as does a `WorkerRef` that is not the bound
/// identity. `None` returns the base assembly. The product factory/config
/// is NOT wired by this function.
pub fn assemble_wasm_isolate_with_managed_database(
    worker_ref: &edger_core::WorkerRef,
    app_secret_resolver: Option<Arc<dyn edger_core::AppSecretResolver>>,
    egress_guard: Option<Arc<EgressGuard>>,
    managed: Option<&ManagedDatabaseEgress>,
) -> WasmIsolate {
    let Some(managed) = managed else {
        return assemble_wasm_isolate(worker_ref, app_secret_resolver, egress_guard);
    };
    let guard = Arc::clone(managed.guard());
    if egress_guard
        .as_ref()
        .is_some_and(|given| !Arc::ptr_eq(given, &guard))
    {
        warn_egress_unavailable_once(
            &worker_ref.name,
            &worker_ref.version,
            "wasm database service transport is unavailable (a second egress guard was given)",
        );
        return assemble_wasm_isolate(worker_ref, app_secret_resolver, egress_guard);
    }
    let isolate = assemble_wasm_isolate(worker_ref, app_secret_resolver, Some(guard));
    let limits = egress_limits_profile(&worker_ref.config.egress);
    let Some(db) = managed.wasm_transport(worker_ref, limits) else {
        warn_egress_unavailable_once(
            &worker_ref.name,
            &worker_ref.version,
            "wasm database service transport is unavailable (worker is not the bound identity)",
        );
        return isolate;
    };
    match &worker_ref.config.egress {
        // General grant: the SAME general field runs the private router.
        Some(settings) => {
            let ctx = Arc::new(EgressWorkerContext {
                worker: worker_ref.name.clone(),
                version: worker_ref.version.clone(),
                namespace: worker_ref.namespace.clone(),
                process_id: String::new(),
            });
            let general = Arc::new(EgressWasmBridge::new(
                Arc::clone(managed.guard()),
                ctx,
                Arc::new(settings.clone()),
            ));
            let router = Arc::new(WasmServiceRouter {
                db_origin: managed.origin().origin(),
                db: Arc::clone(&db),
                general,
            });
            isolate
                .with_http_transport(router)
                .with_service_http_transport(db)
        }
        // DB-only: the private service field ONLY.
        None => isolate.with_service_http_transport(db),
    }
}
