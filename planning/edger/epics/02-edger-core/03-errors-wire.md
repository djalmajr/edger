# Story 02.03: Errors and wire formats (SerializedRequest/Response)

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`  
**Status:** completed (2026-06-29)

## Context
- **Problema:** Isolate boundary needs stable wire types; ad-hoc errors block orchestrator/worker integration.
- **Objetivo:** Implement `SerializedRequest`, `SerializedResponse`, typed `CoreError` domain, header/body limits constants.
- **Valor:** Enables in-process and future multi-process IPC with same types.
- **Restrições:** Pure serde + bytes; no HTTP stack in core.

## Traceability
- **Source docs:** `planning/edger/design.md` (Data Model & Wire Formats)
- **Depende de:** Story 02.01

## Files
| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/wire.rs` | create | SerializedRequest/Response |
| `crates/edger-core/src/error.rs` | create | CoreError + error codes |
| `crates/edger-core/src/lib.rs` | alter | re-exports |
| `crates/edger-core/Cargo.toml` | alter | add `bytes` workspace dep |
| `Cargo.toml` | alter | workspace.dependencies bytes |
| `crates/edger-core/tests/wire_roundtrip.rs` | create | integration tests |

## Detail

### AS-IS
Minimal `CoreError` struct in lib.rs; no wire types.

### TO-BE
Wire types + header limits + roundtrip tests.

### Scope
- In: structs, serde, limit constants, roundtrip tests
- Out: hyper conversion (orchestrator later)

### Acceptance criteria
- [x] JSON roundtrip tests pass for request/response with body
- [x] Empty body serializes correctly
- [x] Error types implement Display + serde where needed

### Dependencies
- Story 02.01

### Pendências
- **bincode/postcard roundtrip:** JSON coberto; framing binário para multi-proc documentado no design — testes bincode adiados para Epic 03 wire story.

## Test-first plan
- **First failing test:** deserialize `SerializedRequest` missing required field fails
- **Level:** `crates/edger-core/tests/wire_roundtrip.rs`

## Tasks
- [x] Add `bytes` to workspace + edger-core (feature `serde`)
- [x] Create `wire.rs` with Serialized* types + derives
- [x] Create `error.rs` with domain error struct per design
- [x] Export from lib.rs with module docs
- [x] Write roundtrip tests (with/without body, binary-safe headers)
- [x] Run `cargo test -p edger-core`

## Verification
```bash
cargo test -p edger-core
cargo clippy -p edger-core -- -D warnings
bun test
```