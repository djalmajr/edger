# Status: Story 02.04 — core traits

**Mode:** Checkpoint  
**Story:** `planning/edger/epics/02-edger-core/04-core-traits.md`

## Completed
- `extension.rs`, `auth.rs`, `isolate.rs`, `context.rs`
- `traits_mock.rs` — 4 tests (middleware chain, auth, handler, isolate)

## Verification
- `cargo test -p edger-core` — 17 tests total
- `rg` no I/O imports in `edger-core/src`

## Pendências
- async-trait for Isolate/WorkerHandler; auth headers as pairs not http::HeaderMap

## Next step
Epic 02 closure; begin Epic 03.01 embedding spike