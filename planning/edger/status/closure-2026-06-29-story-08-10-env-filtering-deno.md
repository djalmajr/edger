# Closure: Story 08.10 Env filtering no Deno CLI bridge

Date: 2026-06-29
Status: completed

## Delivered

- Cleared inherited environment for the Deno CLI child process.
- Preserved only minimal runtime env needed by the CLI (`PATH`/temp/cache vars when present).
- Injected non-sensitive `manifest.env` values into JS/TS workers.
- Reused `edger_core::is_sensitive_env_key` to filter secret-like keys before spawn.
- Added integration coverage through manifest loader, worker pool and Deno bridge.
- Updated value and compatibility matrices.

## Explicit gaps

- `.env` file loading and remote secret stores are still out of scope.
- `envPrefix` expansion from host process remains future work.
- Embedded `deno_core` remains the production target, so the Deno CLI bridge keeps serving as the current execution path.

## Verification

```bash
cargo test -p edger-orchestrator --test kind_dispatch_integration deno_backend_injects_only_filtered_manifest_env
cargo test -p edger-orchestrator --test kind_dispatch_integration
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
