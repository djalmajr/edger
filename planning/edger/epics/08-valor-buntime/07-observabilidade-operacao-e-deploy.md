# Story 08.07: Observabilidade, operação e deploy

**Status:** completed

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** Mesmo com runtime funcional, o valor Buntime inclui operação: health, métricas, logs, request IDs, performance, configuração e práticas de deploy/backup. edger precisa tornar isso explícito.
- **Objetivo:** Consolidar observabilidade e documentação operacional suficiente para rodar edger de modo repetível.
- **Valor:** Operadores conseguem diagnosticar, validar saúde, medir regressão e preservar estado sem depender de leitura do código.
- **Restrições:** Não prometer cluster completo antes da fundação; métricas devem ser leves no hot path; segredos não entram em logs ou docs.

## Traceability
- **Source docs:** `planning/edger/epics/07-avancado/06-observability-otel.md`, `planning/edger/docs/performance-baselines.md`, `planning/edger/docs/value-parity-matrix.md`
- **Buntime refs:** docs locais de performance, security e runtime operations.
- **Prototype refs:** none.
- **Business rules:** evidência operacional precisa ser reproduzível em local dev antes de deploy real.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/metrics.rs` | create | Expor snapshot do pool em Prometheus text format |
| `crates/edger-worker/src/metrics.rs` | edit | Métricas de lifecycle, dispatch e evictions |
| `crates/edger-orchestrator/src/bin/edger.rs` | edit | Health, readiness e metrics endpoint |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Rotas `/healthz`, `/readyz`, `/livez` e `/metrics` no app real |
| `crates/edger-orchestrator/src/server.rs` | edit | Manter router de probes compatível nos testes de servidor |
| `crates/edger-orchestrator/src/lib.rs` | edit | Exportar módulo de metrics |
| `crates/edger-orchestrator/tests/metrics_endpoint.rs` | create | Provar endpoint scrapeável e sem secrets |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Runbook local, health, backup e troubleshooting |
| `planning/edger/docs/performance-baselines.md` | edit | Registrar baseline e comandos |
| `planning/edger/status/evidence/story-08-07-runtime.txt` | create | Registrar evidência runtime manual |
| `planning/edger/docs/value-parity-matrix.md` | edit | Evidência de operação/observabilidade |

## Detail

### AS-IS
- Epic 07 observa OTEL e métricas como fundação, mas operação de produto ainda não está ligada à matriz de valor.
- README mostra comando local; runbook operacional ainda é mínimo.
- Evidence status existe, mas não é suficiente para responder “como operar isso”.

### TO-BE
- `/healthz`, `/readyz` e `/metrics` têm contrato documentado.
- Logs incluem request ID e não expõem segredo.
- Baselines de performance registram cold/warm dispatch e limites conhecidos.
- Runbook documenta start, env vars, health check, backup de estado local e troubleshooting.
- Matriz de valor linka evidências operacionais.

### Scope
- **In:** health/readiness, metrics, runbook local, baseline, evidência versionada.
- **Out:** Helm completo, autoscaling, dashboards finais, SLO formal.

### Story-time plan

**Modo:** plan-then-implement autorizado pelo pedido de continuidade da Epic 8.

**Decisões travadas:**

| Decisão | Escolha | Motivo |
|---|---|---|
| Métricas v1 | `/metrics` em Prometheus text format | Entrega scrape direto sem UI, SSE ou dependência nova |
| Fonte de métricas | `WorkerPool::get_metrics()` | Reutiliza o collector existente e mantém o endpoint read-only |
| Probes | Manter `/health`/`/ready` e adicionar `/healthz`/`/readyz`/`/livez` | Preserva compatibilidade local e aproxima convenção operacional |
| Stats de workers | Fora do v1 | O pool já tem lookup por worker id, mas não lista todos; evitar refatorar LRU nesta fatia |
| Baseline | Local/manual versionado | Primeiro registrar comandos e resultado atual; carga k6/k8s fica para etapa posterior |

**Test-first plan:**

- Primeiro teste novo: `crates/edger-orchestrator/tests/metrics_endpoint.rs` deve provar que `/metrics` retorna `text/plain`, contém counters/gauges do pool e não contém `ROOT_API_KEY`/`authorization`.
- Testes complementares:
  - `/healthz`, `/readyz` e `/livez` respondem com a semântica esperada no pipeline real;
  - request id continua presente em resposta de probe;
  - o worker continua recebendo o mesmo request id via `SerializedRequest`.
- Testes de baixo valor evitados: snapshots longos do Prometheus inteiro; validar apenas nomes e valores contratuais.

### Acceptance criteria
- [x] Health e readiness distinguem processo vivo de runtime pronto.
- [x] Metrics endpoint é scrapeável e não exige secrets em output.
- [x] Logs correlacionam request ID entre orchestrator e worker.
- [x] Runbook local cobre start, env vars, checks, backup e falhas comuns.
- [x] Baseline registra comandos e resultado atual.
- [x] Matriz de valor marca operação com evidência.

### Dependencies
- Epic 07.06 para observabilidade foundation.
- Story 08.03 para request IDs e sanitização.

## Tasks
- [x] Revisar métricas existentes e lacunas.
  - Done when: story documentar que v1 expõe pool snapshot e deixa stats por worker/dashboard fora do escopo.
- [x] Implementar endpoints de health/readiness/metrics se ainda ausentes.
  - Done when: `/health`, `/healthz`, `/ready`, `/readyz`, `/livez` e `/metrics` funcionarem no `build_pipeline`.
- [x] Cobrir endpoint de métricas e aliases operacionais com testes.
  - Done when: teste de integração provar Prometheus text, ausência de secrets e probes.
- [x] Atualizar runbook em docs.
  - Done when: docs cobrirem start, env vars, probes, metrics, backup local e troubleshooting.
- [x] Registrar baseline de performance.
  - Done when: `planning/edger/docs/performance-baselines.md` tiver comandos e resultado atual local.
- [x] Atualizar matriz e status evidence.
  - Done when: matriz apontar evidências e closure registrar verificações.

## Verification
```bash
cargo test -p edger-orchestrator -- metrics
cargo test -p edger-worker -- metrics
ROOT_API_KEY=test-root PORT=19084 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
curl http://127.0.0.1:19084/healthz
curl http://127.0.0.1:19084/readyz
curl http://127.0.0.1:19084/livez
curl http://127.0.0.1:19084/metrics
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
