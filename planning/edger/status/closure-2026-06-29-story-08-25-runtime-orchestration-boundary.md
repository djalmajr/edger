# Closure: Story 08.25 runtime orchestration boundary

Date: 2026-06-29
Status: completed

## Files changed
- `edger-worker/tests/integration_pool.rs`
- `planning/edger/epics/08-valor-buntime/25-runtime-orchestration-boundary.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/docs/compat-matrix.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`
- `planning/edger/status/evidence/story-08-25-runtime.txt`

## Plan status
- [x] Add focused WorkerPool/factory boundary test.
- [x] Update value/compat docs and Epic 08 references.
- [x] Run full verification gates and capture evidence.

## Behavior delivered
- WorkerPool creates isolates through the injected factory with the resolved `WorkerRef`.
- The recorded boundary preserves worker name, namespace, version and inferred `ExecutionKind`.
- `Runtime main-thread orchestration` is now `tested` in the value matrix.

## Verification
See `planning/edger/status/evidence/story-08-25-runtime.txt`.
