# Checkpoint — Story 07.01 Manifests + kinds (WIP)

**Data:** 2026-06-29  
**Story:** `epics/07-avancado/01-full-manifests-kinds.md`

## Entregue

- `edger-orchestrator/src/manifest_loader.rs` — carrega workers de roots ou dirs diretos.
- Fallbacks suportados: `manifest.yaml` / `manifest.yml`, `package.json`, ou `index.{ts,js,mjs,wasm,wat}`.
- `RUNTIME_WORKER_DIRS` (`:`) integrado no bin Rust; default local: `workers`.
- `ManifestIndex` agora resolve worker único com versão `latest`.
- `kind: wasm` explícito preserva `entrypoint`; `.wat` também infere `WasmModule`.
- `workers/wasm-hello/index.wat` — exemplo Wasm textual executável pelo backend wasmtime.

## Evidência

- `cargo test -p edger-orchestrator --test manifest_loader` — 4 testes verdes.
- `cargo test -p edger-orchestrator --test kind_dispatch_integration` — Wasm real via pipeline verde.
- `cargo test -p edger-core --test models_mapping infer_execution_kind_rules` — verde.
- `cargo test -p edger-isolation --features wasm wasm::load` — WAT compila para Wasm.
- Validação manual do bin Rust:
  - `ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger`
  - `GET /ready` → `200 {"status":"ready"}`
  - `GET /wasm-hello` com bearer root → `200 wasm-hello` (backend Wasm real)
  - `GET /hello-world` com bearer root → `200 fetch:GET /` (resolução + dispatch pelo pipeline; JS ainda mock)

## Pendências

| Item | Prioridade |
|---|---|
| Backend embutido via `deno_core` para substituir a bridge Deno CLI | Média |
| Integração E2E por todos os `ExecutionKind` | Alta |
| Documentar semântica de precedência quando múltiplos roots definem o mesmo worker | Média |
