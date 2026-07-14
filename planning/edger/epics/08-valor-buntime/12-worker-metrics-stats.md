# Story 08.12: Stats por worker em métricas operacionais

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz ainda marca `Metrics e pool stats` como `partial` porque `/metrics` expõe apenas counters agregados do pool; operadores não conseguem inspecionar quais workers estão vivos, seus estados e volume de requests.
- **Objetivo:** Entregar um endpoint JSON read-only com snapshot do pool e lista de workers, sem adicionar UI, SSE, retenção histórica ou labels Prometheus de alta cardinalidade.
- **Valor:** Aproxima o edger do valor do plugin de métricas do Buntime: o operador vê pool + workers em um snapshot atual para depuração e dashboards leves.
- **Restrições:** A fonte de verdade permanece o `WorkerPool`; o endpoint não modifica estado, não expõe segredos, e não promete memória por worker enquanto o backend Rust ainda não mede RSS/heap por isolate.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `planning/edger/epics/08-valor-buntime/07-observabilidade-operacao-e-deploy.md`
- **Buntime refs:** `wiki/apps/plugin-metrics.md` em `workspace: zommehq`, `project: buntime`, especialmente `/api/metrics/stats` com pool + workers.
- **Prototype refs:** none; this is an operator API workflow.
- **Business rules:** métricas operacionais são leitura; não devem vazar credenciais, headers de request, env ou corpo de payload.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-worker/src/metrics.rs` | edit | Expand worker stats shape for JSON snapshot |
| `crates/edger-worker/src/instance.rs` | edit | Track worker uptime for stats |
| `crates/edger-worker/src/lru.rs` | edit | Add safe snapshot iteration over cached workers |
| `crates/edger-worker/src/pool.rs` | edit | Expose all worker stats, not only lookup by ID |
| `crates/edger-orchestrator/src/metrics.rs` | edit | Add JSON stats response builder |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Route `/metrics/stats` to read-only JSON handler |
| `crates/edger-orchestrator/tests/metrics_endpoint.rs` | edit | Cover stats endpoint after dispatch and secret hygiene |
| `crates/edger-worker/tests/metrics_ephemeral.rs` | edit | Cover pool-level worker stats snapshot |
| `planning/edger/docs/value-parity-matrix.md` | edit | Update metrics row evidence |
| `planning/edger/docs/compat-matrix.md` | edit | Add technical compatibility row |
| `planning/edger/status/evidence/story-08-12-runtime.txt` | create | Capture commands and results |

## Detail

### AS-IS
- `/metrics` returns Prometheus text exposition for pool-level counters and gauges.
- `WorkerPool::get_worker_stats(worker_id)` can inspect one cached worker by UUID, but there is no operator-facing list.
- `WorkerStats` lacks app identity fields and uptime.

### TO-BE
- `GET /metrics/stats` returns JSON with `pool` and `workers`.
- Each worker row includes `id`, `app`, `name`, `version`, `state`, `requests`, `uptimeSeconds` and `unhealthy`.
- The endpoint is read-only and contains no secrets, env, authorization header, request bodies or raw config.
- Prometheus `/metrics` remains aggregate-only.

### Scope
- **In:** in-process current snapshot, cached workers only, JSON endpoint, tests and matrix/evidence updates.
- **Out:** SSE, UI, memory RSS/heap, historical retention, Prometheus per-worker labels, auth policy changes, cross-process aggregation.

### Approach
- Add `created_at: Instant` to `WorkerInstance` and expose uptime seconds.
- Add LRU snapshot helper returning cloned `Arc<WorkerInstance>` values without holding the LRU lock while reading each instance.
- Add `WorkerPool::worker_stats()` that maps cached instances into stable stat rows.
- Add `metrics_stats_response` in orchestrator metrics code and wire `/metrics/stats`.

### Risks
- **Secret leakage:** avoid serializing `WorkerConfig`, headers or env.
- **Cardinality explosion:** do not add worker-id labels to Prometheus v1.
- **Lock contention:** snapshot should clone instance refs then read fields outside the LRU lock.

### Acceptance criteria
- [x] `GET /metrics/stats` returns `200` JSON with pool counters and worker rows after dispatch.
- [x] Worker rows include identity, state, request count, uptime and unhealthy flag.
- [x] Response does not contain root key, `authorization`, env or request body.
- [x] `/metrics` Prometheus remains text and aggregate-only.
- [x] Matrices no longer claim stats por worker as future-only.

## Test-first plan
- First failing test: dispatch `/echo` twice, then `GET /metrics/stats` returns one worker row with `app: "echo@1.0.0"`, state `idle`, requests `2`, and no secrets.
- Preferred levels:
  - `crates/edger-orchestrator/tests/metrics_endpoint.rs` for API contract and secret hygiene.
  - `crates/edger-worker/tests/metrics_ephemeral.rs` for pool snapshot behavior without HTTP.
- Low-value tests avoided: asserting internal lock behavior or exact uptime value beyond non-negative numeric shape.

## Tasks
- [x] Expand worker stats model and snapshot support.
  - Done when: `WorkerPool::worker_stats()` returns identity/state/request/uptime rows for cached workers.
- [x] Add `/metrics/stats` JSON endpoint.
  - Done when: endpoint returns pool + workers from live `WorkerPool`.
- [x] Add/update tests.
  - Done when: API and pool tests prove observable stats and secret hygiene.
- [x] Update planning artifacts.
  - Done when: matrices, overview, evidence and closure mention 08.12 with explicit out-of-scope items.
- [x] Run focused and workspace gates.
  - Done when: focused tests, Rust gate and planning gate pass.

## Verification
```bash
cargo test -p edger-worker --test metrics_ephemeral
cargo test -p edger-orchestrator --test metrics_endpoint
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
