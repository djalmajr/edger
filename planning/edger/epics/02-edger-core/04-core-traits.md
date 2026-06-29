# Story 02.04: Core traits (Extension, Middleware, Auth, Isolate signatures)

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`

## Context
- **Problem:** Extensions and isolation backends need shared trait contracts in the leaf crate.
- **Objective:** Define traits from design.md with minimal async signatures (or sync where pure); mock-friendly.
- **Value:** Unblocks edger-isolation, edger-orchestrator, edger-ext-* without circular deps.
- **Constraints:** Trait definitions only — no impls beyond test mocks in core.

## Traceability
- **Source docs:** `planning/edger/design.md` (API/Interface Changes, Isolate trait)
- **Depends on:** Stories 02.02, 02.03

## Files
| Path | Action | Reason |
|---|---|---|
| `edger-core/src/extension.rs` | create | Extension, Middleware, WorkerHandler |
| `edger-core/src/auth.rs` | create | AuthProvider, ApiKeyPrincipal helpers |
| `edger-core/src/isolate.rs` | create | Isolate trait + ExecutionKind dispatch types |
| `edger-core/src/lib.rs` | alter | public re-exports |
| `edger-core/tests/traits_mock.rs` | create | compile-time mock impl smoke tests |

## Detail

### AS-IS
No extension/auth/isolate traits.

### TO-BE
- `Extension`, `Middleware`, `WorkerHandler`, `AuthProvider` per design signatures
- `Isolate` trait with `execute_fetch`, `execute_routes`, `serve_static_spa`, `execute_wasm`, lifecycle hooks
- `RequestContext`, `ExtensionContext` stub structs
- Mock types in tests proving traits are object-safe / compile

### Acceptance criteria
- [ ] All traits documented with module-level docs
- [ ] `cargo test -p edger-core` includes trait mock compile tests
- [ ] No `tokio::fs`, `std::fs`, or network imports in edger-core

### Dependencies
- Stories 02.02, 02.03

## Test-first plan
- **First failing test:** mock `Middleware::on_request` returns `None` to continue chain
- **Level:** unit test in `traits_mock.rs`

## Tasks
- [ ] Implement extension.rs traits
- [ ] Implement auth.rs with `principal_can_access_namespace` pure helper
- [ ] Implement isolate.rs trait (async via `async_trait` or manual if needed — document choice)
- [ ] Add RequestContext / ExtensionContext minimal structs
- [ ] Mock impl tests
- [ ] Update AGENTS.md Rust gate note if async_trait added to workspace

## Verification
```bash
cargo test -p edger-core
cargo clippy -p edger-core -- -D warnings
cargo fmt -- --check
bun test
```