# Story 03.02: Implementação completa do trait Isolate + backend mock

**Origin:** `planning/edger/epics/03-isolacao-execucao/00-overview.md`

## Context
- **Problema:** O contrato de execução existe apenas como assinatura em `edger-core`; não há implementação referência nem mock para integração com WorkerPool.
- **Objetivo:** Implementar trait `Isolate` (re-exportado de core) com backend `MockIsolate` que simula todos os caminhos de `ExecutionKind` e lifecycle.
- **Valor:** Desbloqueia Epic 04 (pool chama isolate via trait); permite testes de integração sem V8.
- **Restrições:** `edger-isolation` depende só de `edger-core`; usar `async_trait` ou equivalente documentado; sem I/O real no mock além de filesystem temporário opcional para SPA fixture.

## Traceability
- **Source docs:** `planning/edger/design.md` (Execution Isolation Layer, Isolate trait, ExecutionKind), PR 5
- **Depends on:** Story 03.01 (spike informa shape de erros/limites); Epic 02.04 (trait signatures); Epic 02.02 (WorkerConfig, ExecutionKind)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `crates/edger-isolation/Cargo.toml` | alterar | deps: `edger-core`, `async-trait`, `tokio`, `bytes`, `thiserror` |
| `crates/edger-isolation/src/lib.rs` | criar/alterar | crate root, re-exports |
| `crates/edger-isolation/src/isolate.rs` | criar | trait alias/re-export + tipos auxiliares |
| `crates/edger-isolation/src/mock.rs` | criar | `MockIsolate` impl completa |
| `crates/edger-isolation/src/error.rs` | criar | `IsolationError` mapeando de `CoreError` |
| `crates/edger-isolation/src/kinds.rs` | criar | dispatch por `ExecutionKind` |
| `crates/edger-isolation/tests/mock_isolate.rs` | criar | testes por kind + lifecycle |
| `crates/edger-isolation/src/lib.rs` | alterar | `pub mod mock` feature `testing` ou always-on para dev |

## Detail

### AS-IS
- Trait `Isolate` definido em `edger-core` sem implementação em `edger-isolation`
- Crate isolation vazio ou stub

### TO-BE
- `MockIsolate` com estado interno configurável (respostas canned, contadores de chamadas, flags de falha)
- Métodos async:
  - `execute_fetch` → `SerializedResponse` default 200 + eco de method/uri
  - `execute_routes` → roteamento simulado por prefixo de path
  - `serve_static_spa` → lê fixture HTML de temp dir; injeta `<base href>` se `inject_base`
  - `execute_wasm` → resposta simulada com header `X-Mock-Wasm: 1`
  - `notify_idle` / `terminate` → idempotentes, registram em métricas internas do mock
- `IsolationError` enum: Timeout, MemoryExceeded, ModuleLoad, Wire, Internal
- Helper `dispatch(kind, isolate, req, config)` centralizando match em `ExecutionKind`

### Escopo
- **In:** mock completo, error types, dispatch helper, testes
- **Out:** deno_core real, wasmtime real (story 03.04 prep apenas)

### Critérios de aceite
- [ ] `MockIsolate` implementa todos os métodos do trait `Isolate` de `edger-core`
- [ ] Testes cobrem: FetchHandler, RoutesTable, StaticSpa (com/sem inject_base), WasmModule, Fullstack (stub retorna 501 ou mock adapter)
- [ ] `terminate` chamado duas vezes não panic (idempotente)
- [ ] `cargo test -p edger-isolation` verde
- [ ] Nenhuma dependência em `edger-worker` ou `edger-orchestrator`

### Dependências
- Epic 02.04 (trait)
- Epic 02.02 (WorkerConfig)
- Story 03.01 (opcional para alinhar erros; pode paralelizar após 02.04)

## Test-first plan
- **Primeiro teste falhando:** `mock_isolate_execute_fetch_returns_200` — compila `MockIsolate` e chama `execute_fetch` esperando status 200
- **Nível:** `crates/edger-isolation/tests/mock_isolate.rs` (integration) + unit tests em `mock.rs`
- **Cenários:** falha injetada (`MockIsolate::with_fail_on_terminate`), SPA com base href, wasm kind
- **Evitar:** Mockar tokio runtime inteiro; usar `#[tokio::test]`

## Tasks
- [ ] Configurar `Cargo.toml` de `edger-isolation` com deps workspace
- [ ] Criar `error.rs` com `IsolationError` + `From<CoreError>`
- [ ] Criar `mock.rs` com `MockIsolate` + builder pattern (`with_response`, `with_failures`)
- [ ] Criar `kinds.rs` com `dispatch_execution`
- [ ] Re-exportar `Isolate` trait de `edger-core` em `isolate.rs`
- [ ] Escrever testes por `ExecutionKind` + lifecycle
- [ ] Documentar módulo com exemplos de uso para Epic 04
- [ ] `cargo test -p edger-isolation` + clippy

## Verification
```bash
cargo test -p edger-isolation
cargo test -p edger-isolation --test mock_isolate
cargo clippy -p edger-isolation -- -D warnings
cargo fmt -- --check
bun test
```