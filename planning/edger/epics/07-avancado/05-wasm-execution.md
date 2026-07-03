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
| `edger-isolation/src/wasm/handler.rs` | edit | ABI request/response em linear memory |
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
- WASIp1 é linkado por `wasmtime-wasi` com contexto mínimo: sem preopens de
  filesystem, sem rede por default, env sensível filtrado e stdout/stderr
  opt-in.
- Handler ABI v2 documentado: `memory`, `edger_alloc(len)` e
  `edger_handle(ptr, len)`; request e response trafegam por frames na linear
  memory.
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
  módulos; preopen explícito do worker root para leitura de arquivos locais.

### Acceptance criteria
- [x] Módulo WAT responde via `WasmIsolate::execute_wasm` usando request/response em linear memory.
- [x] Worker `workers/wasm-hello/` ecoa a URI recebida pelo guest em teste in-process de `edger-isolation`.
- [x] Módulo malformado ou path fora do dir falha com `IsolationError` claro.
- [x] WASI não concede acesso a filesystem/rede por default; imports WASIp1 são linkados por host real sem preopens.
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
- [x] `wasi.rs`: deny-by-default + host WASIp1 real sem FS/rede por default.
- [x] Env filter: apenas keys não sensíveis ficam em `WasiConfig`.
- [x] Host WASI real: `wasi_snapshot_preview1` linkado por `wasmtime-wasi`; cap net desabilitada por default.
- [x] Env inject no host WASI: apenas keys permitidas pelo manifest após sensitive filter.
- [ ] Preopen apenas worker root para futuros workers WASI que precisem de filesystem local.

### Fase 3 — Request ABI
- [x] `handler.rs`: serialize request para linear memory; invoke export; deserialize response (ABI v2).
- [x] Documentar ABI v2 em `planning/edger/docs/wasm-abi.md` (curto, versionado).

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
