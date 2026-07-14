# Story 02.04: Core traits (Extension, Middleware, Auth, Isolate signatures)

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`  
**Status:** completed (2026-06-29)

## Context
- **Problema:** Extensions and isolation backends need shared trait contracts in the leaf crate.
- **Objetivo:** Define traits from design.md with minimal async signatures (or sync where pure); mock-friendly.
- **Valor:** Unblocks edger-isolation, edger-orchestrator, edger-ext-* without circular deps.
- **Restrições:** Trait definitions only — no impls beyond test mocks in core.

## Traceability
- **Source docs:** `planning/edger/design.md` (API/Interface Changes, Isolate trait)
- **Depende de:** Stories 02.02, 02.03

## Files
| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/extension.rs` | create | Extension, Middleware, WorkerHandler |
| `crates/edger-core/src/auth.rs` | create | AuthProvider |
| `crates/edger-core/src/isolate.rs` | create | Isolate trait |
| `crates/edger-core/src/context.rs` | create | RequestContext, ExtensionContext |
| `crates/edger-core/src/lib.rs` | alter | public re-exports |
| `crates/edger-core/tests/traits_mock.rs` | create | compile-time mock impl smoke tests |

## Detail

### AS-IS
No extension/auth/isolate traits.

### TO-BE
Traits per design with mock tests.

### Acceptance criteria
- [x] All traits documented with module-level docs (crate-level on lib.rs)
- [x] `cargo test -p edger-core` includes trait mock compile tests
- [x] No `tokio::fs`, `std::fs`, or network imports in edger-core

### Dependencies
- Stories 02.02, 02.03

### Pendências
- **`async-trait`:** usado para `Isolate` e `WorkerHandler` (object-safe + compat 1.80); documentado em `AGENTS.md`.
- **Auth headers:** `AuthProvider::authenticate` usa `&[(String,String)]` em vez de `http::HeaderMap` para manter core sem dep HTTP — orchestrator converte.
- **ExtensionContext:** `logger` / `register_service` substituídos por `serde_json::Value` placeholders até Epic 06.

## Test-first plan
- **First failing test:** mock `Middleware::on_request` returns `None` to continue chain
- **Level:** unit test in `traits_mock.rs`

## Tasks
- [x] Implement extension.rs traits
- [x] Implement auth.rs with `principal_can_access_namespace` pure helper (principal.rs)
- [x] Implement isolate.rs trait via `async_trait`
- [x] Add RequestContext / ExtensionContext minimal structs
- [x] Mock impl tests
- [x] Update AGENTS.md Rust gate note (async-trait)

## Verification
```bash
cargo test -p edger-core
cargo clippy -p edger-core -- -D warnings
cargo fmt -- --check
bun test
```