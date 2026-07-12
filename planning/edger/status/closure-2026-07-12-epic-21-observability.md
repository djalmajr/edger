# Closure 2026-07-12: Epic 21 observability

## Outcome

Epic 21 is complete as a local-first observability capability. EdgeR now provides global and version-scoped observability, bounded events/logs, live tail, console capture, passive health, release/drain lifecycle, optional explicit health checks and OTLP traces/logs without making an external stack mandatory.

## Product boundary

- The cPanel reads EdgeR's local store and pool metrics directly.
- OTLP and Prometheus are optional export/integration surfaces, not cPanel dependencies.
- Worker routing, process state, passive health and explicit probe result remain separate concepts.
- No periodic worker probe was introduced.
- `Settings`/`Deployments` were not added as empty tabs; external Collector health remains owned by the external stack until EdgeR has reliable exporter counters.

## Delivered stories

All stories 21.01–21.12 are completed within the boundary above. Story 21.09 covers safe Helm/Rancher configuration and failure/off/on proof; it deliberately does not claim a Collector-management UI.

## Evidence

- `planning/edger/status/evidence/epic-21-observability-browser-runtime-2026-07-12.md`
- `planning/edger/status/evidence/operational-events-store-2026-07-11.md`
- `planning/edger/status/evidence/logs-explorer-live-tail-2026-07-11.md`
- `planning/edger/status/evidence/worker-passive-health-2026-07-11.md`

## Residual follow-ups

Only evidence-driven extensions remain: durable/cross-replica retention, reliable SDK exporter success/drop counters, or a backend-specific Collector UI. None is required to operate or diagnose a single EdgeR instance.
