# go-wasm-http

Real Go (`wasip1`, stdlib only) over the EdgeR WASM HTTP ABI v1. Routes:

- `/inspect[/...]`: echoes method, request URI, path, raw path, raw query,
  host, the `x-example` header and the body (read limit 64 KiB) as JSON.
- `/db`: runs the fixed probe `SELECT 1 AS value` on SQLD through the host
  `edger_http_v1.fetch` import and answers `{"ok":true,"value":1}`. Static
  `502` for a non-2xx upstream answer or a refused import, static `503` when
  `SQLD_URL` or the `SQLD_TOKEN` secret is missing. Any other path is `404`.

The module has no sockets. Its only outbound path is the host import.

The module imports `edger_http_v1.fetch` for every route, even `/inspect`. What
the manifest needs depends on the process:

- With the managed database configured (the seven `EDGER_SQLD_*` variables), the
  `manifest.yaml` in this directory works as is, with no `allowNet` or `egress`:
  the product binary wraps WASM workers in `ManagedDatabaseIsolate`, which admits
  the import through the private database transport (it reaches only the
  database origin). Without a binding the guest answers `503`
  `configuration missing`; with one it queries the database
  (`go_wasm_app_database_works_end_to_end_and_survives_recycle_and_restart` in
  `crates/edger-orchestrator/tests/sqld_managed_cli_e2e.rs`).
- Without `EDGER_SQLD_*`, a manifest that runs this module needs
  `allowNet: []` and `egress.enabled: true` (see
  `../go-wasm-fullstack/manifest.yaml`). Note that this GRANTS general HTTPS
  egress: the guard does not read `allowNet`, so the guest can reach any HTTPS
  destination the guard accepts (the `egress.ports`, default `[443]`, with IP
  classification, revalidated redirects and the guard limits). `allowNet: []`
  only means the Deno runtime gets no direct network. Without the pair every
  request is refused with `WASM_IMPORT_DENIED`.

A `kind: wasm` worker without `ttl` is also ephemeral and recompiles the module
on every request; declare `ttl` (see `planning/edger/docs/wasm-abi.md`, "Custo do
WASM efêmero").

## Build (offline, Go stdlib only)

From this directory, with a local Go toolchain (no `go.sum`, no network):

```sh
GOOS=wasip1 GOARCH=wasm GOTOOLCHAIN=local GOWORK=off GOPROXY=off \
  go build -trimpath -buildmode=c-shared -ldflags='-s -w' -o index.wasm .
```

The c-shared build is a reactor: it exports `memory`, `edger_alloc`,
`edger_handle` and `_initialize`, and imports `edger_http_v1.fetch`.
`index.wasm` is not committed. The approved test artifact has SHA-256
`8062d9cbed8f48dd8f167d7805b2f83e30e401c0d3a072c9d82dc5f810f535b0`; the
tests refuse any other artifact.

## Real tests (opt-in, WSL Linux)

Both tests are `#[ignore]` and never skip: a missing variable, artifact or
SHA mismatch is a panic. They start a real `sqld` (libsql-server 0.24.32) on
ephemeral loopback ports, with synthetic keys and data in a tempdir.

```sh
EDGER_SQLD_BIN=/path/to/sqld-0.24.32/bin/sqld \
EDGER_GO_WASM_FIXTURE=/path/to/go-ns-host-root-index.wasm \
  cargo test --locked -p edger-orchestrator --test go_wasm_sqld \
  -- --ignored --test-threads=1
```

- `real_go_vault_sqld_namespaces_and_denials`: the isolate run directly
  (vault, scoped JWTs, strict guard, per-namespace SQLD).
- `real_go_through_pipeline_and_pool_with_sqld`: the real `build_pipeline`
  router served by `axum::serve` on `127.0.0.1:0` and called with `reqwest`
  over TCP, backed by the real `WorkerPool` and five apps loaded from disk by
  `load_manifests_from_dirs`. Public requests carry NO `authorization`; the
  host root key stays configured and the test only uses it to show that
  `/api/admin/workers` answers 401 without it and 200 with it. Cases: the
  namespaced app `@nsgo/go-wasm-http` (route `/@nsgo/go-wasm-http/...`, owner
  namespace `@nsgo` as the loader produces it) for `/inspect` and `/db`;
  hostile headers/query that grant nothing; the non-namespaced app
  `go-wasm-http-b` with its own grant; a foreign-owner grant (host denial,
  `502`); a wrong SQLD namespace (upstream `404`, `502`); an app with no grant
  (HTTP 500 with `WASM_IMPORT_DENIED` in the worker error and dispatch event,
  zero egress events). Responses are scanned before diagnostic assertions;
  the shared pipeline/guard event store and `sqld` logs are also scanned for
  fixture secrets. Transport failures use static diagnostics.

Ordinary tests cover token claims, privacy-scan helpers, store pagination and
the log read bound offline. A bounded loopback TCP fault also verifies that a
request query never reaches the transport-failure diagnostic. It starts no child.

## Real limits

- Three levels of proof, kept apart: (1) the direct component test runs the
  isolate in-process, no pool and no HTTP; (2) the pipeline/pool test runs
  HTTP over TCP through the real router and pool but with a TEST
  `IsolateFactory` that calls `assemble_wasm_isolate_with_database` with
  grants the test creates, bound to the exact loaded `WorkerRef` (name,
  version, namespace); (3) the product binary wraps WASM workers in
  `ManagedDatabaseIsolate` when the seven `EDGER_SQLD_*` variables are set,
  and provisioning happens through `PUT /api/admin/workers/{name}/database`
  (see `planning/edger/docs/sqld-managed.md`). The two tests above do not go
  through that path; the launched-binary proof is
  `crates/edger-orchestrator/tests/sqld_managed_cli_e2e.rs`.
- Manifest `secrets`/`env` declare intent only and grant no network. `SQLD_URL`
  is empty in the committed manifest; there is no invented SQLD host.
- No release artifact, CLI command or deploy flow for this worker exists yet.
  Nothing here claims it was published or deployed.
