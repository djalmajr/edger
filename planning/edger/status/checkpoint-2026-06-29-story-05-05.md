# Checkpoint: Story 05.05 — Extension registry

**Date:** 2026-06-29  
**Story:** `epics/05-orquestrador/05-extension-registry.md`  
**Mode:** /agile-status checkpoint

## Progress
- `registry.rs` — `ExtensionRegistry`, priority sort, duplicate rejection
- `hooks.rs` — `run_on_request`, `run_on_response`, lifecycle
- `pipeline.rs` — registry integration, `skip_hooks` for publicRoutes
- `tests/registry_hooks.rs` — 5 tests (short-circuit 418, priority, lifecycle)

## Gates
- `cargo test -p edger-orchestrator --test registry_hooks`: 5 pass
- `cargo test -p edger-orchestrator`: 48 pass

## Next
- Epic 05 closure