# Checkpoint: Story 03.04 — Dual-backend prep

**Date:** 2026-06-29  
**Story:** `epics/03-isolacao-execucao/04-dual-backend-prep.md`  
**Mode:** /agile-status checkpoint

## Progress
- Features `deno` / `wasm` (default `[]`) em `edger-isolation/Cargo.toml`
- Módulos skeleton: `deno/{mod,facade,bundle}.rs`, `wasm/{mod,wasi}.rs`, `backend.rs`
- `DenoIsolate` / `WasmIsolate` impl `Isolate` com `NOT_IMPLEMENTED`
- `ModuleBundler` + `StubBundler` documentados (eszip/precomp → PR 10)
- Factory `create_isolate(IsolationBackend::Mock)` funcional
- Testes: `backend_factory.rs` (2 pass) + 12 isolation tests total

## Fixes colaterais
- `postcard` feature `alloc` em `edger-isolation` (gate `cargo check`)
- `CpuTimer: Default` (clippy)

## Gates
- `cargo test -p edger-isolation`: 14 pass
- `cargo check --features deno,wasm`: OK
- `cargo clippy --workspace -D warnings`: OK
- `bun test`: 6 pass

## Next
- Epic 03 closure → Epic 04.01 WorkerPool + LRU