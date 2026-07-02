# Story 07.06: Observabilidade (tracing, OTEL, /metrics)

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`

## Context
- **Problema:** Runtime sem visibilidade operacional: logs ad-hoc, sem correlação `request_id`, sem export Prometheus/OTEL, métricas de pool/isolate ausentes para produção.
- **Objetivo:** Instrumentar orchestrator, worker e isolation com `tracing`,
  preparar configuração OpenTelemetry sem falhar startup, expor endpoint
  `/metrics` Prometheus, e propagar `X-Request-Id` em todo o pipeline.
- **Valor:** Debug de isolates, SLOs, integração com dashboards (padrão Buntime
  `plugin-metrics` -> futuro `edger-ext-metrics`).
- **Restrições:** Sem vazar secrets em spans; sampling via env; overhead controlado no hot path.

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 12, Observability section)
- **Design PR:** PR 12 (parcial — observabilidade)
- **Buntime refs:** `X-Request-Id`, pool metrics, worker lifecycle events
- **Depende de:** 07.01, 07.04, 07.05 (spans úteis em execução real)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/tracing_init.rs` | create | Subscriber fmt, env filter and non-fatal OTEL env parsing |
| `edger-orchestrator/src/metrics.rs` | edit | HTTP counters/duration plus existing pool/cron exposition |
| `edger-orchestrator/src/pipeline.rs` | edit | Dispatch logs and request pipeline correlation |
| `edger-orchestrator/src/server.rs` | edit | Request-id propagation and HTTP metrics middleware for base router |
| `edger-worker/src/pool.rs` | edit | Instrument `pool.fetch` and isolate execution dispatch spans |
| `edger-orchestrator/src/bin/edger.rs` | edit | Use tracing init from env at startup |
| `edger-orchestrator/tests/metrics_endpoint.rs` | edit | Scrape `/metrics`, generated request-id propagation and log redaction tests |

## Detail

### AS-IS
- O binário inicializava `tracing_subscriber` inline com filtro mínimo.
- `/metrics`, `/metrics/stats`, pool metrics e cron metrics já existiam por
  fatias anteriores, mas faltavam contadores HTTP do orchestrator.
- O middleware devolvia `x-request-id`, mas quando gerava um ID novo ele não o
  inseria no request antes do dispatch; o worker podia observar outro ID.
- Logs de dispatch não carregavam `request_id` + `worker_name` de forma
  explícita.
- Não havia inicialização centralizada para `EDGER_LOG`, `RUST_LOG` ou envs
  `OTEL_*`.

### TO-BE
- Startup: subscriber fmt centralizado, `EDGER_LOG` preferido sobre `RUST_LOG`,
  default seguro para crates `edger_*`, e envs `OTEL_*` aceitos sem falhar.
- Cada request HTTP preserva ou gera `x-request-id` antes do dispatch e ecoa o
  mesmo valor na resposta.
- Logs de dispatch incluem `request_id`, `worker_name`, `worker_version` e
  `worker_namespace`, sem headers/body.
- Spans leves cobrem `pool.fetch` e `isolate.execute` com worker/kind.
- Métricas Prometheus:
  - `edger_http_requests_total{status, method}`
  - `edger_http_request_duration_ms_last`
  - métricas existentes de pool/cache/spawn/request duration/ephemeral
  - `edger_cron_executions_total` e `edger_cron_failures_total`
- `/metrics` retorna text exposition format.
- Response inclui `X-Request-Id` echo.

### Scope
- **In:** tracing/log correlation, non-fatal OTEL env parsing, Prometheus
  `/metrics`, request_id correlation, pool/isolate spans, pool metrics alignment.
- **Out:** OTLP exporter layer linked into this binary, Dashboard Grafana
  bundled, `edger-ext-metrics` crate completa, distributed tracing multi-proc
  full.

### Acceptance criteria
- [x] Request sem `X-Request-Id` recebe ID gerado; response header devolve o mesmo valor.
- [x] Logs estruturados incluem `request_id` e `worker_name` quando dispatch ocorre.
- [x] `GET /metrics` retorna 200 com pelo menos `edger_http_requests_total` após um request.
- [x] Com `OTEL_EXPORTER_OTLP_ENDPOINT` mock/disabled, startup não falha.
- [x] Nenhum span/log contém valores de `Authorization` ou API keys (redaction).
- [x] `pool.get_metrics()` alinhado com Prometheus counters.
- [ ] Com endpoint OTLP válido, spans exportados; pendente até linkar
  `tracing-opentelemetry`/exporter no workspace.

### Dependencies
- Stories 07.01, 07.04, 07.05 (pipeline real para métricas significativas)
- Story 07.03 opcional para métrica cron

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`edger-orchestrator/tests/`, `edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Tracing foundation
- [x] `tracing_init.rs`: subscriber com env filter `RUST_LOG` / `EDGER_LOG`.
- [x] Gerar/propagar `request_id` no início do pipeline.
- [x] Log estruturado no dispatch com correlação segura.

### Fase 2 — Worker + isolation spans
- [x] Instrument `pool.fetch` com worker/version/namespace.
- [x] Instrument dispatch para isolate por `ExecutionKind`.
- [ ] Spans detalhados de spawn/retire ficam para o hardening/perf slice.

### Fase 3 — Metrics endpoint
- [x] `metrics.rs`: registrar contador HTTP e duração last-observed.
- [x] Montar middleware de métricas no server e pipeline antes do catch-all worker routes.

### Fase 4 — OTEL + testes
- [x] Config parser aceita `OTEL_EXPORTER_OTLP_ENDPOINT` e `OTEL_TRACES_SAMPLER`
  sem falhar startup.
- [x] Testes: request_id header, metrics scrape, log redaction/correlation.
- [x] Doc operacional: variáveis de ambiente observabilidade.
- [ ] Layer `tracing-opentelemetry` com sampling real segue follow-up.

## Verification
```bash
cargo test -p edger-orchestrator -- metrics_endpoint
cargo test -p edger-orchestrator -- request_id
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
