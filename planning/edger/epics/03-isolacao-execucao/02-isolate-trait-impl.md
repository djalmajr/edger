# Story 03.02: Implementação completa do trait Isolate + backend mock

**Origin:** `planning/edger/epics/03-isolacao-execucao/00-overview.md`  
**Status:** completed (2026-06-29)

## Context
- **Problema:** O contrato de execução existe apenas como assinatura em `edger-core`; não há implementação referência nem mock para integração com WorkerPool.
- **Objetivo:** Implementar trait `Isolate` (re-exportado de core) com backend `MockIsolate` que simula todos os caminhos de `ExecutionKind` e lifecycle.
- **Valor:** Desbloqueia Epic 04 (pool chama isolate via trait); permite testes de integração sem V8.
- **Restrições:** `edger-isolation` depende só de `edger-core`; usar `async_trait` ou equivalente documentado; sem I/O real no mock além de filesystem temporário opcional para SPA fixture.

## Traceability
- **Source docs:** `planning/edger/design.md` (Execution Isolation Layer, Isolate trait, ExecutionKind), PR 5
- **Depende de:** Story 03.01 (spike informa shape de erros/limites); Epic 02.04 (trait signatures); Epic 02.02 (WorkerConfig, ExecutionKind)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-isolation/Cargo.toml` | alterar | deps: `edger-core`, `async-trait`, `tokio`, `bytes`, `thiserror` |
| `edger-isolation/src/lib.rs` | criar/alterar | crate root, re-exports |
| `edger-isolation/src/isolate.rs` | criar | trait alias/re-export + tipos auxiliares |
| `edger-isolation/src/mock.rs` | criar | `MockIsolate` impl completa |
| `edger-isolation/src/error.rs` | criar | `IsolationBackendError` mapeando de `CoreError` |
| `edger-isolation/src/kinds.rs` | criar | dispatch por `ExecutionKind` |
| `edger-isolation/tests/mock_isolate.rs` | criar | testes por kind + lifecycle |

## Detail

### AS-IS
- Trait `Isolate` definido em `edger-core` sem implementação em `edger-isolation`
- Crate isolation vazio ou stub

### TO-BE
- `MockIsolate` com estado interno configurável
- `dispatch_execution` helper
- `IsolationBackendError` enum

### Escopo
- **In:** mock completo, error types, dispatch helper, testes
- **Out:** deno_core real, wasmtime real (story 03.04 prep apenas)

### Critérios de aceite
- [x] `MockIsolate` implementa todos os métodos do trait `Isolate` de `edger-core`
- [x] Testes cobrem: FetchHandler, RoutesTable, StaticSpa (com inject_base), WasmModule, Fullstack (501)
- [x] `terminate` chamado duas vezes não panic (idempotente)
- [x] `cargo test -p edger-isolation` verde (7 tests)
- [x] Nenhuma dependência em `edger-worker` ou `edger-orchestrator`

### Pendências
- SPA fixture via filesystem temp — usado HTML in-memory no mock; FS opcional adiado.

## Test-first plan
- Red: `mock_isolate_execute_fetch_returns_200` falhou sem impl
- Green: `mock.rs` + `kinds.rs`
- Refactor: `dispatch_execution` centralizado

## Tasks
- [x] Configurar `Cargo.toml` de `edger-isolation` com deps workspace
- [x] Criar `error.rs` com `IsolationBackendError` + `From<CoreError>`
- [x] Criar `mock.rs` com `MockIsolate` + builder (`with_spa_html`, `with_fail_on_terminate`)
- [x] Criar `kinds.rs` com `dispatch_execution`
- [x] Re-exportar `Isolate` trait de `edger-core` em `isolate.rs`
- [x] Escrever testes por `ExecutionKind` + lifecycle
- [x] `cargo test -p edger-isolation` + clippy

## Verification
```bash
cargo test -p edger-isolation
cargo test -p edger-isolation --test mock_isolate
cargo clippy -p edger-isolation -- -D warnings
cargo fmt -- --check
bun test
```