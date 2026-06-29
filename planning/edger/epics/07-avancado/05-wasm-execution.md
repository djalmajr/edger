# Story 07.05: Execução Wasm standalone (wasmtime + WASI)

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`

## Context
- **Problema:** `ExecutionKind::WasmModule` só existe no mock; decisão do usuário exige wasmtime + WASI standalone, separado do isolate JS deno_core.
- **Objetivo:** Implementar `WasmIsolate` (ou módulo `wasm.rs`) com wasmtime que carrega `.wasm` do worker dir, expõe handler HTTP via convenção WASI/http ou export nomeado, integrado ao pool.
- **Valor:** Workers Wasm deployáveis com isolamento capability-based; completude do surface de app types do Buntime.
- **Restrições:** Não co-localizar Wasm no V8 isolate; validação rigorosa de módulo; WASI capabilities deny-by-default; prep multi-process via wire types.

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 10 Wasm path, Security Wasm, Resolved Decisions)
- **Design PR:** PR 10 (parte Wasm)
- **Depende de:** Epic 03 (spike wasmtime prep), Epic 04 (pool), Epic 05 (dispatch)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/wasm.rs` | create | `WasmIsolate` impl `Isolate::execute_wasm` |
| `edger-isolation/src/wasm/wasi.rs` | create | Config WASI: stdin/stdout/env caps limitados |
| `edger-isolation/src/wasm/handler.rs` | create | Convenção entry export + request/response ABI |
| `edger-isolation/src/lib.rs` | edit | Registrar backend Wasm; feature `wasm` |
| `edger-isolation/Cargo.toml` | edit | deps `wasmtime`, `wasmtime-wasi` |
| `edger-worker/src/instance.rs` | edit | Selecionar `WasmIsolate` quando kind `WasmModule` |
| `edger-isolation/tests/wasm_integration.rs` | create | Módulo mínimo responde HTTP |
| `workers/wasm-hello/` | create | `manifest.yaml` kind wasm + `index.wasm` ou build script |
| `workers/wasm-hello/build.rs` ou `Makefile` | create | Compilar fixture Rust→wasm para testes |

## Detail

### AS-IS
- `execute_wasm` no mock retorna resposta sintética fixa.
- Spike pode ter comparado wasmtime mas sem integração pool.
- Sem validação de capabilities WASI.

### TO-BE
- Entrypoint `.wasm` do manifest carregado em wasmtime Engine com config determinística.
- WASI: apenas dirs/files do worker sandbox; env filtrado (mesmos padrões sensíveis Buntime).
- Handler: convenção documentada — ex. export `handle_request(ptr, len)` ou WASI HTTP preview; adapter traduz `SerializedRequest` → Wasm → `SerializedResponse`.
- `WasmModule { entry: Option<String> }` no `ExecutionKind` honrado.
- Supervisor trata Wasm instance lifecycle separado de V8 (memória contabilizada por engine).
- Teste integração com módulo compilado em CI (tiny wasm from Rust `cdylib`).

### Scope
- **In:** wasmtime standalone, WASI sandbox, execute_wasm, pool wiring, fixture + testes.
- **Out:** Wasm dentro do deno isolate; component model avançado; hot reload de módulos.

### Acceptance criteria
- [ ] Worker `workers/wasm-hello/` com kind `wasm` responde GET via pool com body determinístico.
- [ ] Módulo malformado ou path fora do dir falha com `IsolationError` claro.
- [ ] WASI não concede acesso a filesystem fora do worker dir (teste negativo).
- [ ] Env vars `*_SECRET` não passam para Wasm (filtro portado).
- [ ] Coexistência: processo pode ter isolates V8 e Wasm simultâneos sem shared mutable state.
- [ ] `cargo test -p edger-isolation --features wasm` verde.

### Dependencies
- Epic 03 — spike wasmtime prep
- Epic 04 — WorkerPool spawn path por kind
- Pode paralelizar com 07.04 (sem dependência mútua)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`edger-orchestrator/tests/`, `edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Engine + load
- [ ] `wasm.rs`: Engine/Store/Module load from worker dir entrypoint.
- [ ] Validação: tamanho máximo módulo, magic bytes, reject unknown imports per policy.

### Fase 2 — WASI sandbox
- [ ] `wasi.rs`: preopen apenas worker root; cap net desabilitada por default.
- [ ] Env inject: apenas keys permitidas pelo manifest após sensitive filter.

### Fase 3 — Request ABI
- [ ] `handler.rs`: serialize request para linear memory; invoke export; deserialize response.
- [ ] Documentar ABI em `planning/edger/docs/wasm-abi.md` (curto, versionado).

### Fase 4 — Integração
- [ ] Wire `WasmIsolate` no worker instance selection.
- [ ] Build fixture wasm em `workers/wasm-hello/` (script documentado).
- [ ] Integration test + gate workspace.

## Verification
```bash
cargo test -p edger-isolation --features wasm
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
bun test
```
