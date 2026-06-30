# Checkpoint: Epic 08 value parity

Date: 2026-06-29
Status: value-parity proof complete for currently executable edger flows

## Completed stories

- 08.01 defined the value-parity matrix and non-copy rule.
- 08.02 delivered operational API inventory and protected admin surfaces.
- 08.03 delivered operational security boundaries.
- 08.04 delivered local SQL/KV/queue service contracts.
- 08.05 delivered shell/gateway v1.
- 08.06 delivered extension providers, hooks, capabilities and binding lookup.
- 08.07 delivered probes, `/metrics`, baseline and operation runbook.
- 08.08 delivered migration proof suite, Browser `/todos` validation and the
  `base: ""` route-hijack guard learned from Buntime.
- 08.09 delivered API key create/revoke by Admin API.
- 08.10 delivered safe env injection for JS/TS workers through the Deno bridge.
- 08.11 delivered runtime worker enable/disable with in-memory overlay.
- 08.12 delivered `/metrics/stats` JSON with pool + worker rows.
- 08.13 delivered runtime extension enable/disable with real hook/provider effect.
- 08.14 delivered manifest-less `index.html` autodiscovery with HTML priority.
- 08.15 delivered gateway redirect rules with suffix/query preservation.
- 08.16 delivered gateway local rate limiting with per-client buckets.
- 08.17 delivered local gateway operational diagnostics in the root extension
  inventory.
- 08.18 delivered read-only gateway Admin API endpoints for stats, config and
  filtered logs.
- 08.19 delivered aggregated stats for recent gateway logs.
- 08.20 delivered real duration tracking for gateway logs and log stats.
- 08.21 delivered local gateway rate-limit metrics through a dedicated root-only
  Admin API endpoint.
- 08.22 delivered semver range routing for namespaced and unscoped workers.
- 08.23 delivered a file-backed API key bootstrap store proof independent of
  durable SQL providers.
- 08.24 delivered explicit CSRF/internal-call mutation coverage that does not
  elevate non-root keys.
- 08.25 delivered a runtime orchestration boundary proof through WorkerPool
  factory/isolate dispatch.
- 08.26 delivered optional extension enable/disable status persistence through
  a JSON status store without changing the explicit v1 registration model.
- 08.27 delivered a mechanical deploy/layout checker for the local operation
  runbook and wired it into the planning gate.
- 08.28 delivered worker lifecycle hooks around real `WorkerPool` dispatch.
- 08.29 delivered structured operational error logs for Admin API and pipeline
  failures without leaking secrets.

## Evidence

- `edger-orchestrator/tests/value_parity.rs`
- `workers/value-parity/todos/`
- `planning/edger/status/evidence/story-08-08-runtime.txt`
- `planning/edger/status/evidence/story-08-09-runtime.txt`
- `planning/edger/status/evidence/story-08-10-runtime.txt`
- `planning/edger/status/evidence/story-08-11-runtime.txt`
- `planning/edger/status/evidence/story-08-12-runtime.txt`
- `planning/edger/status/evidence/story-08-13-runtime.txt`
- `planning/edger/status/evidence/story-08-14-runtime.txt`
- `planning/edger/status/evidence/story-08-15-runtime.txt`
- `planning/edger/status/evidence/story-08-16-runtime.txt`
- `planning/edger/status/evidence/story-08-17-runtime.txt`
- `planning/edger/status/evidence/story-08-18-runtime.txt`
- `planning/edger/status/evidence/story-08-19-runtime.txt`
- `planning/edger/status/evidence/story-08-20-runtime.txt`
- `planning/edger/status/evidence/story-08-21-runtime.txt`
- `planning/edger/status/evidence/story-08-22-runtime.txt`
- `planning/edger/status/evidence/story-08-23-runtime.txt`
- `planning/edger/status/evidence/story-08-24-runtime.txt`
- `planning/edger/status/evidence/story-08-25-runtime.txt`
- `planning/edger/status/evidence/story-08-26-runtime.txt`
- `planning/edger/status/evidence/story-08-27-runtime.txt`
- `planning/edger/status/evidence/story-08-28-runtime.txt`
- `planning/edger/status/evidence/story-08-29-runtime.txt`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/docs/compat-matrix.md`

## Value proven

- SPA/TodoMVC-equivalent document, asset and fallback routing under `/todos`.
- Protected worker auth boundary.
- State bindings for SQL, KV and queue descriptors.
- Durable state provider boundary: local SQL/KV/queue remain proven in Epic 08,
  while Turso remote/sync moved to Epic 09 as an external provider dependency.
- Shell document routing plus iframe app bypass.
- Gateway CORS/auth behavior.
- Pool metrics and operation probes from Story 08.07.
- Worker stats snapshot in `/metrics/stats`.
- Manifest-less static SPA autodiscovery with `index.html`.
- API key create/revoke without raw secret leakage after creation.
- Deno manifest env injection with sensitive env filtering.
- Runtime worker enable/disable without process restart.
- Runtime extension enable/disable without process restart.
- Gateway prefix redirects with path suffix and query string preserved.
- Gateway local rate limit with 429 and operational headers.
- Gateway local operational diagnostics with counters and recent decisions.
- Gateway read-only Admin API for stats, config and filtered logs.
- Gateway read-only Admin API for aggregated recent log stats.
- Gateway log durations and average duration in read-only log stats.
- Gateway read-only Admin API for local rate-limit metrics without exposing
  bucket keys.
- Worker addressing resolves `latest`, exact versions and semver ranges without
  treating exact versions as implicit caret ranges.
- API key bootstrap auth uses its own file-backed store: synthetic root auth and
  persisted key auth work before/without durable SQL provider registration.
- Admin mutations enforce same-origin for browser requests and keep
  `x-edger-internal` as a root-only CSRF bypass, not an auth/elevation mechanism.
- The runtime main process resolves routing/config and dispatches through
  WorkerPool/factory/isolate; JS and Wasm execution stay outside orchestrator code.
- Extension enable/disable status can be persisted in an optional JSON status
  store and reloaded by a rebuilt registry.
- Local operation/deploy layout is gate-checked: worker roots, local state,
  extension status store, probes, Admin API, backup and required gates must stay
  documented.
- Extension hooks now cover worker lifecycle around real dispatch:
  request hook, worker dispatch, worker complete and response hook ordering is
  tested, and request short-circuit skips worker lifecycle hooks.
- Operational errors now emit structured `edger.operational` warnings for
  Admin API and pipeline failures with surface, request ID, status and code,
  while tests prove Authorization, raw tokens, body and error messages do not
  leak.
- Empty plugin `base: ""` no longer registers a shell/app surface.

## Explicit remaining gaps

- Native cron scheduler remains pending in Epic 07.03.
- Embedded `deno_core` remains the production target; Deno CLI bridge is the
  current execution path.
- Proxy external forwarding, cache, persistent/distributed rate limiting,
  gateway-specific SSE/history, bucket list/reset and dynamic mutation APIs
  remain partial.
- Durable external providers, including Turso remote/sync, are tracked by Epic 09
  instead of being an internal Epic 08 implementation requirement.
- Retry/backoff/DLQ depth, dynamic extension reload/rescan, full extension
  manifest persistence, deploy remoto/PVC/K8s, UI administration and marketplace
  remain future work.

## Verification to keep green

```bash
cargo test -p edger-orchestrator --test value_parity
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
