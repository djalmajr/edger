# Matriz de compatibilidade Buntime ↔ edger

**Status:** atualizado pela Story 08.25 com fronteira de orquestração runtime e lacunas explícitas
**Origin:** `planning/edger/design.md` (Migration notes, mapping table)

Legenda: `pending` = ainda não testado | `tested` | `partial` | `gap`

Paridade de valor fica em `planning/edger/docs/value-parity-matrix.md`. Esta matriz responde se o comportamento técnico é compatível; a matriz de valor responde se o fluxo operacional/produto equivalente está entregue.

| Comportamento Buntime | edger | Status | Teste / notas |
|---|---|---|---|
| Worker addressing `/name`, `/@scope/name@ver`, ranges semver | orchestrator router | tested | `edger-orchestrator/tests/routing_resolution.rs`; `edger-orchestrator/tests/value_parity.rs`; suporta `latest`, versão exata e `semver::VersionReq` como `^1.0.0` e `~1.2.0` |
| Runtime orchestration boundary | worker pool + isolation factory | tested | `edger-worker/tests/integration_pool.rs`; `edger-orchestrator/tests/kind_dispatch_integration.rs`; `WorkerPool` recebe `WorkerRef` resolvido, cria isolate por factory injetada e despacha pelo kind inferido; o orchestrator não executa código de app diretamente |
| Manifest fields (mapping table design) | edger-core manifest | partial | `edger-core/tests/models_mapping.rs`; complex Buntime fields still tracked by value matrix |
| Entrypoint autodiscovery priority | manifest loader | tested | `edger-orchestrator/tests/manifest_loader.rs`; `manifest.yaml`/`package.json` permanecem fontes explícitas, e autodiscovery sem manifesto escolhe `index.html` antes de `index.ts/js/mjs` |
| `fetch(req) -> Response` contract | Deno CLI bridge in isolation | tested | `edger-orchestrator/tests/kind_dispatch_integration.rs` |
| Worker-local `deno.json` / import map | Deno CLI bridge cwd + `--config` | tested | `deno_backend_loads_worker_deno_config_import_map`; `serve` manual curl |
| Response body finite stream | Deno CLI bridge buffers response | tested | `chunked-text` E2E |
| Infinite/SSE stream | Deno CLI bounded first chunk | partial | `stream` and `sse` return first chunk; true streaming passthrough still pending |
| Remote `https://` imports | Deno CLI bridge | tested | `logger-stdout` manual curl returned 200; cache/network policy hardening still pending |
| CommonJS Node server examples | Node `http.createServer` adapter | partial | Simple `commonjs` tested; Buntime-compatible mounted paths strip worker base and expose `x-base`, so standalone Hono route `/commonjs-hono` resolves at `/commonjs-hono/commonjs-hono` |
| Mounted worker base path | Relative request path + `x-base` | tested | Mirrors Buntime `createWorkerRequest`; namespaced worker test asserts `/api/ping` + `x-base: /@team/checkout` |
| `routes` export | isolation | pending | E2E 07.04 |
| Static file read from JS worker | Deno CLI bridge | tested | `serve-html` E2E |
| Static SPA (`entrypoint: index.html`) | Rust `serve_static_spa` | tested | `edger-orchestrator/tests/manifest_loader.rs`; `workers` fixture + manual `buntime/apps/todos` HTTP validation: manifest-less discovery, index/assets/favicon/fallback + `<base href="/todos/">` |
| `visibility: public` worker access | auth pipeline bypass | tested | `todos` manual no-auth HTTP validation + `public_visibility_worker_bypasses_auth` |
| ApiKeyPrincipal + namespaces | auth gate | tested | `edger-orchestrator/tests/auth_gate.rs`; `edger-orchestrator/tests/security_operational.rs`; `edger-orchestrator/tests/value_parity.rs` |
| Root key bypass | auth | tested | `edger-ext-auth/tests/auth_provider.rs`; `edger-orchestrator/tests/admin_workers_plugins.rs`; `edger-orchestrator/tests/value_parity.rs` |
| Admin API key create/revoke | auth/admin API | tested | `edger-orchestrator/tests/admin_workers_plugins.rs`; `edger-ext-auth/tests/auth_provider.rs`; criação retorna `rawKey` uma vez e revogação invalida autenticação |
| API key file-backed bootstrap store | auth store | tested | `edger-ext-auth/tests/auth_provider.rs`; `EDGER_AUTH_DB` usa `SqliteApiKeyStore::open(path)`, que autentica root synthetic principal e chave persistida, depois reabre o arquivo e autentica a chave sem depender de `DurableSqlProvider`, Turso remoto/sync ou registry de providers |
| Admin mutation CSRF/internal guard | orchestrator admin API | tested | `edger-orchestrator/tests/security_operational.rs`; `edger-orchestrator/tests/admin_workers_plugins.rs`; browser-originated mutations exigem same-origin, CLI/API com Bearer funciona sem `Origin`, e `x-edger-internal: true` não autentica nem eleva keys não-root |
| Worker runtime enable/disable | manifest index/admin API | tested | `edger-orchestrator/tests/admin_workers_plugins.rs`; `edger-orchestrator/tests/security_operational.rs`; `edger-orchestrator/tests/routing_resolution.rs`; overlay em memória remove worker disabled da rota e inventário mostra `disabled`/`loaded` |
| Extension runtime enable/disable | extension registry/admin API | tested | `edger-orchestrator/tests/admin_workers_plugins.rs`; `edger-orchestrator/tests/registry_hooks.rs`; `edger-orchestrator/tests/registry_providers.rs`; overlay em memória atualiza inventário, pula middleware desabilitado e remove provider desabilitada de binding lookup |
| publicRoutes bypass | auth + hooks | tested | `edger-orchestrator/tests/auth_gate.rs`; `workers/value-parity/todos` Browser no-auth validation |
| Sliding TTL / ephemeral ttl=0 | worker pool | tested | `edger-worker/tests/supervisor_lifecycle.rs`; `edger-worker/tests/metrics_ephemeral.rs`; `edger-worker/tests/integration_pool.rs` |
| maxRequests cap | supervisor | tested | `edger-worker/tests/supervisor_lifecycle.rs`; `edger-worker/tests/metrics_ephemeral.rs` |
| onRequest hooks order + short-circuit | extension registry | tested | `edger-orchestrator/tests/registry_hooks.rs`; `edger-orchestrator/tests/value_parity.rs`; gateway preflight is auth-gated before hook short-circuit |
| Reserved paths `/api`, `/health` | router | tested | `edger-orchestrator/tests/routing_resolution.rs`; `edger-orchestrator/tests/shell_gateway.rs`; `edger-orchestrator/tests/value_parity.rs` |
| Empty plugin `base: ""` | manifest index + shell resolver | tested | `edger-orchestrator/tests/value_parity.rs`; empty base does not register a shell/app surface |
| Env filtering sensitive patterns | worker env | tested | `edger-isolation` WASI filtering test; `edger-orchestrator/tests/kind_dispatch_integration.rs`; Deno child env is cleared before spawn and only filtered manifest env is injected |
| Worker stats snapshot | worker pool + metrics API | tested | `edger-worker/tests/metrics_ephemeral.rs`; `edger-orchestrator/tests/metrics_endpoint.rs`; `/metrics/stats` returns pool + workers JSON, while `/metrics` remains aggregate Prometheus text |
| Cron internal requests | native scheduler 07.03 | tested | `edger-orchestrator/tests/cron_scheduler_test.rs`; scheduler snapshots enabled `manifest.cron[]` jobs at startup, validates schedule v1, dispatches in-process HTTP with `x-edger-internal: true` + root bearer credential at the pipeline boundary, strips root auth before worker serialization, exposes `edger_cron_*` Prometheus counters, and cancels tasks on shutdown; full cron grammar/distributed leader election remain outside v1 |
| Shell / micro-frontend | shell routing 07.02 | tested | `edger-orchestrator/tests/shell_gateway.rs`; `edger-orchestrator/tests/value_parity.rs` |
| Gateway redirect rules | gateway middleware | tested | `edger-ext-gateway/tests/gateway_middleware.rs`; `edger-orchestrator::wire` and pipeline tests preserve query through serialization and worker path rewrite; prefix rules short-circuit with 301/302/307/308, preserve path suffix and query string, and CORS preflight remains 204 |
| Gateway local/persistent rate limit | gateway middleware + durable SQL | tested | `edger-ext-gateway/tests/gateway_middleware.rs`; `edger-orchestrator/tests/state_services.rs`; token bucket local por cliente bloqueia com 429, `x-ratelimit-*` e `retry-after`; preflight não consome bucket; modo persistente opcional usa `DurableSqlProvider`, persiste hash da chave do bucket e sobrevive reconstrução do módulo; distribuição multi-região fica fora do v1 |
| Gateway operational diagnostics | extension inventory + gateway middleware | tested | `edger-ext-gateway/tests/gateway_middleware.rs`; `edger-orchestrator/tests/admin_workers_plugins.rs`; `Extension::diagnostics()` é opcional, `/api/admin/extensions` expõe snapshot root-only para o gateway, contadores e ring buffer local não serializam headers/body/segredos |
| Gateway read-only Admin API | admin API + gateway diagnostics | tested | `edger-orchestrator/tests/admin_workers_plugins.rs`; `/api/admin/gateway/stats`, `/api/admin/gateway/config`, `/api/admin/gateway/logs`, `/api/admin/gateway/logs/stream`, `/api/admin/gateway/logs/stats` e `/api/admin/gateway/rate-limit/metrics` são root-only, reutilizam `Extension::diagnostics()`, suportam filtros, stream SSE dos eventos redigidos, agregados, duração média dos logs e métricas locais/persistentes de rate limit, e continuam sem buckets/reset ou mutações |
