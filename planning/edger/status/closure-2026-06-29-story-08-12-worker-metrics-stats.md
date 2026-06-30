# Closure: Story 08.12 worker metrics stats

Date: 2026-06-29

Story 08.12 delivered read-only worker stats for the operational metrics surface. The edger now exposes `/metrics/stats` with pool counters plus cached worker rows, while keeping `/metrics` as aggregate Prometheus text.

## Delivered
- `WorkerInstance` tracks uptime from creation.
- `WorkerPool::worker_stats()` returns a safe snapshot of cached workers without holding the LRU lock during per-instance reads.
- `WorkerPool::fetch_worker()` preserves manifest-resolved worker identity for orchestrator dispatch and stats.
- `/metrics/stats` returns JSON with pool counters and workers containing `id`, `app`, `name`, `version`, `namespace`, `state`, `requests`, `uptimeSeconds` and `unhealthy`.
- Tests cover pool snapshot behavior, HTTP JSON contract, Prometheus aggregate-only behavior and secret hygiene.

## Explicitly not delivered
- SSE metrics streaming.
- Built-in metrics UI.
- Worker memory RSS/heap measurements.
- Historical retention or Prometheus/Grafana setup.
- Per-worker Prometheus labels.
- Cross-process aggregation.

## Verification
```bash
cargo test -p edger-worker --test metrics_ephemeral
cargo test -p edger-orchestrator --test metrics_endpoint
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining follow-up
- Continue remaining value gaps from `planning/edger/docs/value-parity-matrix.md`: cron, embedded `deno_core`, streaming, gateway proxy/cache/rate-limit persistence, Turso remote/sync, retry/DLQ depth and persistent extension reload.
