# Status: Story 03.02 — MockIsolate

**Mode:** Checkpoint  
**Story:** `planning/edger/epics/03-isolacao-execucao/02-isolate-trait-impl.md`

## Completed
- `MockIsolate` implements all `Isolate` methods
- `dispatch_execution` for all `ExecutionKind` variants
- `IsolationBackendError` in `error.rs`
- 7 integration tests in `mock_isolate.rs`

## Verification
- `cargo test -p edger-isolation` — 7 pass
- workspace + bun — pass

## Next step
Story 03.03 — wire + limits