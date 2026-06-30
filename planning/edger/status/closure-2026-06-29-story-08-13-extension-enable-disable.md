# Closure: Story 08.13 extension enable/disable runtime

Date: 2026-06-29

Story 08.13 delivered runtime extension enable/disable through the Admin API without copying Buntime's dynamic loader or persisting manifests in this slice. Root callers can disable and re-enable a registered extension, inventory reports `enabled` or `disabled`, middleware hooks stop running while disabled, and service providers stop satisfying binding lookup while disabled.

## Delivered
- `AdminExtensionInfo` now includes `status`.
- `ExtensionRegistry` keeps the static registered list and applies an in-memory shared status overlay.
- Hooks use active middlewares only.
- Provider accessors return providers only while their extension is enabled.
- `POST /api/admin/extensions/{name}/enable|disable` uses the existing root and CSRF/internal-call mutation guard.
- Tests cover API protection, inventory status, middleware runtime effect and provider lookup effect.

## Explicitly not delivered
- Persisting extension status to manifest files.
- Uploading plugin packages.
- Rescanning extension directories.
- Dynamic topological reload.
- Native route table reload.
- Admin UI or marketplace.

## Verification
```bash
cargo test -p edger-orchestrator --test registry_hooks
cargo test -p edger-orchestrator --test registry_providers
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining follow-up
- Continue remaining value gaps from `planning/edger/docs/value-parity-matrix.md`: cron, embedded `deno_core`, streaming, gateway proxy/cache/rate-limit persistence, Turso remote/sync, retry/DLQ depth and persistent extension reload.
