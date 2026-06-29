# Story 07.06: Observabilidade (tracing, OTEL, /metrics)

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`

## Context
- **Problema:** Runtime sem visibilidade operacional: logs ad-hoc, sem correlação `request_id`, sem export Prometheus/OTEL, métricas de pool/isolate ausentes para produção.
- **Objetivo:** Instrumentar orchestrator, worker e isolation com `tracing`, export OpenTelemetry configurável, endpoint `/metrics` Prometheus, e propagação de `X-Request-Id` em todo o pipeline.
- **Valor:** Debug de isolates, SLOs, integração com dashboards (padrão Buntime `plugin-metrics` → futuro `edger-ext-metrics`).
- **Restrições:** Sem vazar secrets em spans; sampling via env; overhead controlado no hot path.

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 12, Observability section)
- **Design PR:** PR 12 (parcial — observabilidade)
- **Buntime refs:** `X-Request-Id`, pool metrics, worker lifecycle events
- **Depende de:** 07.01, 07.04, 07.05 (spans úteis em execução real)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/tracing_init.rs` | create | Subscriber fmt + OTEL layer, env config |
| `edger-orchestrator/src/metrics.rs` | create | Counters/histograms + handler `/metrics` |
| `edger-orchestrator/src/pipeline.rs` | edit | Spans `request`, `auth`, `hooks`, `dispatch`; inject `request_id` |
| `edger-orchestrator/src/server.rs` | edit | Rota `/metrics`, `/health`, `/ready`, `/live` enriquecidos |
| `edger-worker/src/metrics.rs` | edit | Export pool hit rate, spawn latency, active workers, ephemeral queue |
| `edger-worker/src/pool.rs` | edit | Instrument get_or_create, TTL retire |
| `edger-isolation/src/lib.rs` | edit | Spans `isolate.execute` com worker_id + kind |
| `edger-core/src/wire.rs` | edit | Garantir `request_id` em `SerializedRequest` |
| `Cargo.toml` (workspace) | edit | `tracing`, `tracing-subscriber`, `tracing-opentelemetry`, `opentelemetry`, `prometheus` ou `metrics` crate |
| `edger-orchestrator/tests/metrics_endpoint_test.rs` | create | Scrape `/metrics` contém counters esperados |
| `edger-orchestrator/tests/request_id_test.rs` | create | Header `X-Request-Id` na response |

## Detail

### AS-IS
- `tracing_subscriber::fmt::init()` mínimo no sketch do design.
- Sem OTEL exporter, sem `/metrics`, sem spans por isolate.
- `request_id` pode não propagar até o worker.

### TO-BE
- Startup: layer stack — fmt (dev) + OTEL (prod, `OTEL_EXPORTER_OTLP_ENDPOINT`).
- Cada request HTTP: span root `http_request` com `request_id` (gerar UUID se ausente no header).
- Child spans: `auth.resolve`, `hook.on_request`, `router.resolve`, `pool.fetch`, `isolate.execute` (attributes: `worker.name`, `execution_kind`, `namespace`).
- Métricas Prometheus:
  - `edger_http_requests_total{status, method}`
  - `edger_http_request_duration_seconds` histogram
  - `edger_pool_workers_active`, `edger_pool_hit_total`, `edger_pool_spawn_duration_seconds`
  - `edger_isolate_errors_total{kind}`
  - `edger_cron_executions_total` (prep from 07.03)
- `/metrics` retorna text exposition format; auth: internal ou desabilitado em dev (documentado).
- Response inclui `X-Request-Id` echo.
- Lifecycle events (`on_spawn`, `on_terminate`) logados em INFO com worker_id.

### Scope
- **In:** tracing spans, OTEL export, Prometheus `/metrics`, request_id correlation, pool metrics.
- **Out:** Dashboard Grafana bundled; `edger-ext-metrics` crate completa; distributed tracing multi-proc full.

### Acceptance criteria
- [ ] Request sem `X-Request-Id` recebe ID gerado; response header devolve o mesmo valor.
- [ ] Logs estruturados incluem `request_id` e `worker_name` quando dispatch ocorre.
- [ ] `GET /metrics` retorna 200 com pelo menos `edger_http_requests_total` após um request.
- [ ] Com `OTEL_EXPORTER_OTLP_ENDPOINT` mock/disabled, startup não falha; com endpoint válido, spans exportados (teste opcional com collector em docker-compose doc).
- [ ] Nenhum span/log contém valores de `Authorization` ou API keys (redaction).
- [ ] `pool.get_metrics()` alinhado com Prometheus counters.

### Dependencies
- Stories 07.01, 07.04, 07.05 (pipeline real para métricas significativas)
- Story 07.03 opcional para métrica cron

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`edger-orchestrator/tests/`, `edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Tracing foundation
- [ ] `tracing_init.rs`: subscriber com env filter `RUST_LOG` / `EDGER_LOG`.
- [ ] Gerar/propagar `request_id` no início do pipeline.
- [ ] Spans no orchestrator (auth, hooks, router).

### Fase 2 — Worker + isolation spans
- [ ] Instrument `pool.fetch`, spawn, retire.
- [ ] Isolate: span por `ExecutionKind` com duration.

### Fase 3 — Metrics endpoint
- [ ] `metrics.rs`: registrar counters/histograms; HTTP handler.
- [ ] Montar rota em server antes do catch-all worker routes.

### Fase 4 — OTEL + testes
- [ ] Layer `tracing-opentelemetry` com sampling `OTEL_TRACES_SAMPLER`.
- [ ] Testes: request_id header, metrics scrape.
- [ ] Doc em README: variáveis de ambiente observabilidade.

## Verification
- `cargo test -p edger-orchestrator -- metrics_endpoint`
- `cargo test -p edger-orchestrator -- request_id`
- Manual: `curl -v /any` → header `X-Request-Id`; `curl /metrics | grep edger_`
- `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check`
- `bun test`