# Closure: Story 08.14 manifest-less index.html autodiscovery

Date: 2026-06-29

Story 08.14 delivered Buntime-compatible entrypoint autodiscovery for static SPAs without requiring a `manifest.yaml`. A worker directory containing only `index.html` is now discovered, and when `index.html` coexists with `index.ts` in a manifest-less worker, HTML wins and the worker is treated as `StaticSpa`.

## Delivered
- `index.html` is the first conventional entrypoint candidate.
- Direct worker directories with only `index.html` are recognized as worker dirs.
- Root worker discovery recognizes manifest-less static SPA directories.
- Tests cover HTML priority over JS and direct worker-dir discovery.
- Value and compatibility matrices now mark worker manifest/entrypoint detection as tested for this contract.

## Explicitly not delivered
- Semver range expansion beyond current router behavior.
- Worker upload/install/remove.
- Watch mode or hot reload.
- Package manager integration.
- Changes to JS/TS execution.

## Verification
```bash
cargo test -p edger-orchestrator --test manifest_loader
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Remaining follow-up
- Continue remaining value gaps from `planning/edger/docs/value-parity-matrix.md`: gateway proxy/cache/rate-limit persistence, cron execution, Turso remote/sync, retry/DLQ depth, persistent extension reload and deeper logging/performance harnesses.
