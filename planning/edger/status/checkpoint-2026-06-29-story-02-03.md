# Status: Story 02.03 — errors + wire

**Mode:** Checkpoint  
**Story:** `planning/edger/epics/02-edger-core/03-errors-wire.md`

## Completed
- `wire.rs` SerializedRequest/Response + header limits
- `error.rs` CoreError + IsolationError
- `bytes` workspace dep with serde feature
- `wire_roundtrip.rs` — 5 tests

## Verification
- `cargo test -p edger-core` — 13 tests cumulative

## Pendência
- bincode/postcard IPC roundtrips — Epic 03 wire story

## Next step
Story 02.04 — core traits