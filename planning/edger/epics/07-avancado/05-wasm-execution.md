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
| `edger-isolation/src/wasm/mod.rs` | edit | `WasmIsolate` impl `Isolate::execute_wasm` |
| `edger-isolation/src/wasm/wasi.rs` | edit | Config WASI deny-by-default + env filter |
| `edger-isolation/src/wasm/handler.rs` | edit | Convenção ABI v1 `http_status` + `http_body_len` |
| `edger-isolation/src/wasm/load.rs` | edit | Load seguro `.wasm`/`.wat` dentro do worker dir |
| `edger-isolation/src/lib.rs` | edit | Registrar backend Wasm; feature `wasm` |
| `edger-isolation/Cargo.toml` | edit | deps `wasmtime`, `wasmtime-wasi`, `wat` |
| `edger-orchestrator/src/bin/edger.rs` | edit | Factory runtime seleciona `WasmIsolate` para `WasmModule` |
| `edger-worker/tests/wasm_pool_integration.rs` | edit | Pool dispatch real para Wasm |
| `edger-orchestrator/tests/kind_dispatch_integration.rs` | edit | Pipeline real e coexistência JS/Wasm |
| `workers/wasm-hello/` | edit | `manifest.yaml`, `index.wat` e README de fixture |

## Detail

### AS-IS
- Antes da fatia v1, `execute_wasm` no mock retornava resposta sintética fixa.
- Spike comparava wasmtime, mas não havia integração pool/pipeline real.
- Sem validação de módulo, path ou capabilities WASI no caminho de runtime.

### TO-BE
- Entrypoint `.wasm` ou `.wat` do manifest carregado em wasmtime Engine com
  config determinística e validação de path sob worker dir.
- WASI v1 deny-by-default: imports WASI/host são rejeitados; env sensível é
  filtrado antes de qualquer futura injeção.
- Handler ABI v1 documentado: `memory`, `http_status()` e `http_body_len()`;
  response body é lido do offset `0`.
- `WasmModule { entry: Option<String> }` no `ExecutionKind` é honrado pelo pool
  e pela factory dinâmica do binário.
- Processo/pool consegue servir workers JS/TS e Wasm no mesmo runtime sem estado
  mutável compartilhado entre backends.
- Fixture `workers/wasm-hello` usa `index.wat` versionado e documenta como
  materializar `index.wasm`.

### Scope
- **In:** wasmtime standalone, WASI deny-by-default, execute_wasm, pool/bin
  wiring, fixture + testes, coexistência JS/Wasm.
- **Out:** Wasm dentro do deno isolate; component model avançado; hot reload de
  módulos; host WASI real com preopen; request/response ABI completa via
  linear memory.

### Acceptance criteria
- [x] Módulo WAT mínimo responde via `WasmIsolate::execute_wasm` (ABI v1: `http_status` + `http_body_len`)
- [x] Worker `workers/wasm-hello/` responde GET via `pool.fetch`/pipeline com body `wasm-hello` (tests `wasm_pool_integration.rs`, `kind_dispatch_integration.rs`)
- [x] Módulo malformado ou path fora do dir falha com `IsolationError` claro.
- [x] WASI não concede acesso a filesystem fora do worker dir (imports WASI bloqueados no ABI v1).
- [x] Env vars `*_SECRET` não passam para Wasm (`WasiConfig` filtra env antes de futura injeção).
- [x] Coexistência: processo pode ter isolates V8 e Wasm simultâneos sem shared mutable state.
- [x] `cargo test -p edger-isolation --features wasm` verde.

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
- [x] `wasm/handler.rs`: Engine/Store/Module load from bytes (WAT→wasm)
- [x] Load from worker dir entrypoint via manifest
- [x] Entrypoint `.wat` compila para bytes Wasm para fixtures/exemplos locais.
- [x] Validação: tamanho máximo módulo, magic bytes, reject unknown imports per policy.

### Fase 2 — WASI sandbox
- [x] `wasi.rs`: deny-by-default + imports WASI/host bloqueados no ABI v1.
- [x] Env filter: apenas keys não sensíveis ficam em `WasiConfig`.
- [ ] Host WASI real: preopen apenas worker root; cap net desabilitada por default (follow-up pós-ABI v1).
- [ ] Env inject no host WASI: apenas keys permitidas pelo manifest após sensitive filter (follow-up pós-ABI v1).

### Fase 3 — Request ABI
- [ ] `handler.rs`: serialize request para linear memory; invoke export; deserialize response (follow-up ABI v2).
- [x] Documentar ABI v1 em `planning/edger/docs/wasm-abi.md` (curto, versionado).

### Fase 4 — Integração
- [x] `WorkerPool::fetch` respeita `WorkerConfig.kind` quando `kind_hint` não é informado.
- [x] Factory dinâmica do orquestrador Rust seleciona `WasmIsolate` para `WasmModule`.
- [x] Build fixture wasm em `workers/wasm-hello/` documentado.
- [x] Integration test `wasm_pool_integration.rs` verde.

## Verification
```bash
cargo test -p edger-isolation --features wasm
cargo test -p edger-worker --test wasm_pool_integration
cargo test -p edger-orchestrator --test kind_dispatch_integration wasm
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
