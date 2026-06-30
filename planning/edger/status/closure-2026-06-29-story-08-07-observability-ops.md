# Closure: Story 08.07 Observabilidade, operação e deploy

Date: 2026-06-29
Status: completed

## Delivered

- Added Prometheus text exposition at `/metrics` from the live `WorkerPool`
  snapshot.
- Added operational probe aliases: `/healthz`, `/readyz`, `/livez`, while
  preserving `/health` and `/ready`.
- Preserved/generated `x-request-id` on probe responses and propagated the same
  header into worker dispatch.
- Added integration coverage for metrics content type, secret-free output,
  pool cache hit/miss reflection, probe aliases and worker request-id
  propagation.
- Documented local operation: env vars, launch checks, probes, metrics, backup
  and troubleshooting.
- Captured a first local performance baseline and linked operational evidence
  into the value parity matrix.
- Validated probes and metrics through the in-app Browser.

## Explicit gaps

- `/metrics` v1 exposes pool-level metrics only; per-worker listing/stats stay
  out of this slice.
- Load harness, dashboards, OpenTelemetry export and SLOs remain future work.
- Backup guidance is local/single-node; PVC/Kubernetes runbooks stay out of
  scope for this story.
- `hello-world` runtime sample is ephemeral (`ttl=0`), so cache hit evidence is
  intentionally covered by the automated TTL test instead.

## Evidence

- `edger-orchestrator/tests/metrics_endpoint.rs`
- `edger-orchestrator/src/metrics.rs`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/docs/performance-baselines.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/status/evidence/story-08-07-runtime.txt`

## Verification

```bash
cargo test -p edger-orchestrator --test metrics_endpoint
cargo test -p edger-orchestrator metrics
cargo test -p edger-worker -- metrics
ROOT_API_KEY=test-root PORT=19084 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
curl http://127.0.0.1:19084/healthz
curl http://127.0.0.1:19084/readyz
curl http://127.0.0.1:19084/livez
curl http://127.0.0.1:19084/metrics
```

Full workspace gate to run before final claim:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
