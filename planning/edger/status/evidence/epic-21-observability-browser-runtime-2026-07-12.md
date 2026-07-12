# Evidence: Epic 21 observability runtime and Browser

Date: 2026-07-12

## Runtime

Launched with:

```bash
ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
```

Real requests were sent to `health-demo` and the intentionally failing `boom-ui`. The root-only series endpoint returned a bounded 5-minute window with 17 requests, 4 errors, p95 76 ms and `partialWindow=true` after the fresh runtime start.

The manual worker check returned:

```json
{"worker":"health-demo","version":"1.0.0","path":"/health","method":"GET","trigger":"manual","healthy":true,"status":200,"durationMs":1,"code":null,"message":"Health check completed successfully"}
```

## Browser proof

Validated in the bundled in-app Browser against `http://127.0.0.1:19080`:

- session survived direct navigation and refresh;
- `/cpanel/observability` rendered request rate from a rolling 60-second window, p95, errors, processes, queue pressure, three bounded charts and partial-window copy;
- `/cpanel/workers/health-demo/1.0.0/observability` showed routing, passive health, process/request/p95 cards, scoped charts, capacity and the explicit `Run health check` action;
- running the check rendered `GET /health · 200 · 1 ms` while the passive sample count and request total remained unchanged;
- `/cpanel/workers/health-demo/1.0.0/logs` showed sanitized `health_check` events with source, outcome, status, duration and request ID;
- the logs level menu rendered above the informational alert (`z-index: 50`) and remained fully legible;
- the attention popover opened with actionable reason, stayed within the viewport, used bounded `overflow-y: auto` and closed after an outside click;
- a 768×900 responsive viewport had no document-level horizontal overflow; worker version fields flowed as a grid and the sidebar collapsed to 64 px;
- the viewport override was reset after verification.

## OTLP and deploy proof

```bash
cargo test -p edger-orchestrator --features otel tracing_init::tests
cargo clippy -p edger-orchestrator --all-targets --features otel -- -D warnings
helm lint charts/edger
helm template edger charts/edger > /tmp/edger-otel-off.yaml
helm template edger charts/edger --set otel.enabled=true --set otel.endpoint=http://collector:4318 > /tmp/edger-otel-on.yaml
```

The local receiver observed both `/v1/traces` and `/v1/logs`; payload assertions found the sanitized event envelope and `[redacted]`, and rejected the original secret/path. A separate unavailable-collector test kept the local event query working and observed the exporter error during flush.

`helm template` with `otel.enabled=true` and no endpoint failed with `otel.endpoint is required when otel.enabled=true`. The generated enabled manifest contained `EDGER_OTEL_ENABLED`, endpoint and protocol, while secret headers remain an existing-Secret reference.

## Health gate proof

- manual health check is root-only and creates an operational event;
- the synthetic request does not increment `request_total` or passive-health `sample_count`;
- successful on-deploy check promotes the candidate;
- failed on-deploy check returns `DEPLOY_HEALTH_CHECK_FAILED`, keeps the pathname unroutable and removes the rejected package from disk so it cannot return after restart/rescan.
