# Closure: Story 08.11 worker enable/disable runtime

Date: 2026-06-29

Story 08.11 delivered runtime worker enable/disable through the Admin API without copying Buntime internals or persisting manifests in this slice. Root callers can disable and re-enable a loaded worker, inventory reports `disabled` or `loaded`, and route resolution stops dispatching disabled workers.

## Delivered
- `ManifestIndex` now shares mutable runtime status across cloned orchestrator state.
- `POST /api/admin/workers/{name}/disable` and `/enable` run through the existing root and CSRF/internal-call guard.
- Disabled workers are filtered from direct worker resolution, plugin base routing, homepage and shell lookups.
- Tests cover API mutation, inventory status, denied mutation attempts, same-origin/internal allowed mutations, and route-level `NOT_FOUND`.

## Explicitly not delivered
- Manifest file persistence.
- Install/remove worker APIs.
- Hot reload of files or registry refresh.
- Per-version mutation selection.
- Multi-process replication of runtime overlay state.

## Verification
```bash
cargo test -p edger-orchestrator --test routing_resolution
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test -p edger-orchestrator --test security_operational
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining follow-up
- Continue the remaining value gaps from `planning/edger/docs/value-parity-matrix.md`: cron, embedded `deno_core`, streaming, gateway proxy/cache/rate-limit persistence, Turso remote/sync, retry/DLQ depth, persistent extension reload and marketplace.
