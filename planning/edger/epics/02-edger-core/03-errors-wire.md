# Story 02.03: Errors and wire formats (SerializedRequest/Response)

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`

## Context
- **Problem:** Isolate boundary needs stable wire types; ad-hoc errors block orchestrator/worker integration.
- **Objective:** Implement `SerializedRequest`, `SerializedResponse`, typed `CoreError` domain, header/body limits constants.
- **Value:** Enables in-process and future multi-process IPC with same types.
- **Constraints:** Pure serde + bytes; no HTTP stack in core.

## Traceability
- **Source docs:** `planning/edger/design.md` (Data Model & Wire Formats)
- **Depends on:** Story 02.01

## Files
| Path | Action | Reason |
|---|---|---|
| `edger-core/src/wire.rs` | create | SerializedRequest/Response |
| `edger-core/src/error.rs` | create | CoreError + error codes |
| `edger-core/src/lib.rs` | alter | re-exports |
| `edger-core/Cargo.toml` | alter | add `bytes` workspace dep |
| `Cargo.toml` | alter | workspace.dependencies bytes |
| `edger-core/tests/wire_roundtrip.rs` | create | integration tests |

## Detail

### AS-IS
Minimal `CoreError` struct in lib.rs; no wire types.

### TO-BE
- `SerializedRequest { method, uri, headers, body: Option<Bytes>, request_id, base_href }`
- `SerializedResponse { status, headers, body }`
- Header limit constants (100 headers, 64KiB total, 8KiB per value) as pure constants
- Error types for parse/validation (no I/O errors)

### Scope
- In: structs, serde, limit constants, roundtrip tests
- Out: hyper conversion (orchestrator later)

### Acceptance criteria
- [ ] JSON/bincode roundtrip tests pass for request/response with body
- [ ] Empty body serializes correctly
- [ ] Error types implement Display + serde where needed

### Dependencies
- Story 02.01

## Test-first plan
- **First failing test:** deserialize `SerializedRequest` missing required field fails
- **Level:** `edger-core/tests/wire_roundtrip.rs`
- **Avoid:** Mocking HTTP; test pure serde only

## Tasks
- [ ] Add `bytes` to workspace + edger-core
- [ ] Create `wire.rs` with Serialized* types + derives
- [ ] Create `error.rs` with domain error enum/struct per design
- [ ] Export from lib.rs with module docs
- [ ] Write roundtrip tests (with/without body, binary-safe headers)
- [ ] Run `cargo test -p edger-core`

## Verification
```bash
cargo test -p edger-core
cargo clippy -p edger-core -- -D warnings
bun test
```