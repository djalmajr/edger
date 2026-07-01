# wasm-hello fixture

`index.wat` is the checked-in source for the Story 07.05 Wasm fixture. The
runtime can load `.wat` directly for local development and tests by compiling it
to Wasm bytes before validation.

To materialize `index.wasm` manually when `wasm-tools` is available:

```bash
wasm-tools parse index.wat -o index.wasm
```

Keep `manifest.yaml` pointing at `index.wat` unless a test or release artifact
explicitly needs precompiled Wasm bytes.
