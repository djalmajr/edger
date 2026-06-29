# Status: Story 03.01 — embedding spike

**Mode:** Checkpoint  
**Story:** `planning/edger/epics/03-isolacao-execucao/01-embedding-spike.md`

## Completed
- `embedding-spike-wasm` example — compile_ms=10, invoke_ms=0
- `embedding-spike-deno` wire sim (SerializedRequest roundtrip)
- `spike.md` preenchido com go/no-go
- `edger-isolation` depends on `edger-core`; wasmtime dev-deps

## Pendência
- Full deno_core V8 boot + fetch roundtrip — story 03.04 / feature `deno`

## Verification
- `cargo test --workspace` — pass
- `cargo run --example embedding-spike-wasm` — pass

## Next step
Story 03.02 — isolate trait mock impl