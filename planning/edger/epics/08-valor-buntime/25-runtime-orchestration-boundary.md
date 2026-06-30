# Story 08.25: Fronteira de orquestração runtime

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz ainda marcava `Runtime main-thread orchestration` como `partial`, apesar de haver dispatch JS/Wasm e pipeline via pool. Faltava uma prova explícita da fronteira: o processo principal resolve/coordena, mas quem recebe o `WorkerRef` e executa é o pool/isolate.
- **Objetivo:** Provar que o `WorkerPool` cria isolates por factory injetada com o `WorkerRef` resolvido, preservando namespace, versão e `ExecutionKind` antes do dispatch.
- **Valor:** A arquitetura entrega o mesmo valor operacional do Buntime, mas de forma Rust-native: o orchestrator não embute execução de app code.
- **Restrições:** Não reintroduzir Bun adapter, não mover Deno/Wasm para o orchestrator e não prometer embedded `deno_core` como fechado enquanto o Deno CLI bridge ainda é o caminho atual.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `planning/edger/runtime-functional-plan.md`.
- **Buntime refs:** valor de runtime main-thread orchestration referenciado no Epic 08.
- **Prototype refs:** none; this is a runtime boundary contract.
- **Business rules:** worker code must execute behind isolation/pool boundaries, not inside admin/router code.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-worker/tests/integration_pool.rs` | edit | Add contract test proving factory receives resolved `WorkerRef` and dispatches by `ExecutionKind` |
| `planning/edger/docs/value-parity-matrix.md` | edit | Move runtime orchestration boundary to tested |
| `planning/edger/docs/compat-matrix.md` | edit | Add technical compatibility row for orchestration boundary |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | edit | Register Story 08.25 and update status |
| `planning/edger/roadmap.md` | edit | Update Epic 8 story count |
| `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md` | edit | Record the new closed value slice |
| `planning/edger/status/closure-2026-06-29-story-08-25-runtime-orchestration-boundary.md` | create | Closure report for the story |
| `planning/edger/status/evidence/story-08-25-runtime.txt` | create | Command evidence for focused and full gates |

## Detail

### AS-IS
- `edger-orchestrator` resolves workers and calls `WorkerPool`.
- JS/TS workers execute through `DenoIsolate`/Deno CLI bridge in integration tests.
- Wasm workers execute through `WasmIsolate` in pool tests.
- The value matrix still lacked a single evidence line proving the resolved `WorkerRef` boundary.

### TO-BE
- `integration_factory_receives_resolved_worker_ref_before_dispatch` records the `WorkerRef` passed to the injected factory and asserts name, namespace, version and `ExecutionKind::WasmModule`.
- The matrix marks runtime main-thread orchestration as tested, with current caveat that embedded `deno_core` remains a production target tracked elsewhere.

### Scope
- **In:** worker pool/factory boundary, `WorkerRef` identity, `ExecutionKind` inference, dispatch through isolate.
- **Out:** embedded `deno_core` completion, multi-process clustering, replacing the Deno CLI bridge.

### Approach
- Use a test factory that records the `WorkerRef` and returns `MockIsolate`.
- Dispatch a namespaced Wasm worker via `fetch_worker` without `kind_hint`, so the test proves the resolved config drives isolate dispatch.
- Reference existing Deno/Wasm integration tests for real execution engines.

### Risks
- **Overclaiming:** This closes orchestration boundary, not embedded `deno_core` or multi-process process supervision.
- **False positive:** The test asserts response prefix and recorded `ExecutionKind`, so dropping the kind before dispatch or bypassing the factory would fail.

### Acceptance criteria
- [x] Pool factory receives resolved worker name, namespace, version and Wasm `ExecutionKind`.
- [x] Dispatch response proves execution went through the isolate path.
- [x] `Runtime main-thread orchestration` is `tested` in the value matrix with evidence.
- [x] Rust and planning gates are green.

## Test-first plan
- First failing test: `integration_factory_receives_resolved_worker_ref_before_dispatch` in `edger-worker/tests/integration_pool.rs`.
- Preferred level: WorkerPool integration test with injected factory.
- Observable behavior: response body comes from isolate dispatch and recorded factory input preserves resolved worker metadata.
- Low-value tests avoided: type-name assertions for production isolates, direct private helper tests, or duplicating full JS/Wasm integration setup.

## Tasks
- [x] Add focused WorkerPool/factory boundary test.
  - Done when: focused `edger-worker` integration test passes.
- [x] Update value/compat docs and Epic 08 references.
  - Done when: `Runtime main-thread orchestration` is `tested` without claiming embedded `deno_core` is complete.
- [x] Run full verification gates and capture evidence.
  - Done when: Rust gate, planning gate and diff check are recorded in `story-08-25-runtime.txt`.

## Verification
```bash
cargo test -p edger-worker --test integration_pool integration_factory_receives_resolved_worker_ref_before_dispatch
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
git diff --check
```
