# Story 07.05 Closure: Execução Wasm standalone

## Summary

Story 07.05 is complete for the standalone wasmtime v1 boundary. Wasm workers
now execute through `WasmIsolate`, load worker-local `.wasm` or `.wat`
entrypoints safely, expose a documented HTTP response ABI, and dispatch through
the same worker pool and orchestrator pipeline as JS/TS workers.

## Delivered

- Added real wasmtime execution for `ExecutionKind::WasmModule`.
- Added worker-dir loading for `.wasm` and `.wat` entrypoints with path escape
  rejection.
- Added module validation for magic bytes, size cap, host imports and WASI
  imports.
- Added deny-by-default `WasiConfig` with sensitive env filtering for future
  injection.
- Documented ABI v1 in `planning/edger/docs/wasm-abi.md`.
- Confirmed the runtime factory selects `WasmIsolate` for Wasm workers.
- Added an orchestrator integration test proving Deno and Wasm workers can run
  from the same process/pool.
- Added `workers/wasm-hello/README.md` to document the fixture and optional
  `index.wasm` materialization.
- Updated Epic 07, pendency tracking, compat matrix, value matrix and roadmap.

## Evidence

- `cargo test -p edger-isolation --features wasm` passed.
- `cargo test -p edger-worker --test wasm_pool_integration` passed.
- `cargo test -p edger-orchestrator --test kind_dispatch_integration wasm`
  passed.
- `cargo fmt -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`
  passed.
- `planning/edger/status/evidence/story-07-05-runtime.txt` records the covered
  behavior and workspace test caveat.

## Follow-up

- Implement host WASI with worker-root-only preopen and no network by default.
- Add request/response linear-memory ABI for dynamic HTTP request handling.
- Continue the active roadmap sequence with Story 07.07 Hardening + matriz
  compat.
