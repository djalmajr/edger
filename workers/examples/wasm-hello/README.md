# wasm-hello fixture

`index.wat` is the checked-in source for the Wasm fixture. The runtime can load
`.wat` directly for local development and tests by compiling it to Wasm bytes
before validation.

The fixture implements the EdgeR linear-memory HTTP ABI:

- `edger_alloc(len: i32) -> i32` allocates request bytes.
- `edger_handle(ptr: i32, len: i32) -> i64` receives the request frame and
  returns packed response pointer/length.
- The response body echoes the request URI as `wasm path: <uri>`, proving the
  request crossed into the guest.

To materialize `index.wasm` manually when `wasm-tools` is available:

```bash
wasm-tools parse index.wat -o index.wasm
```

Keep `manifest.yaml` pointing at `index.wat` unless a test or release artifact
explicitly needs precompiled Wasm bytes.
