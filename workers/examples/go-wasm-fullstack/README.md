# go-wasm-fullstack

A Vite single-page app and a Go (`wasip1`) backend in one EdgeR package
(`kind: fullstack`, `adapter: wasm`). The SPA is served from `dist/`; every
request under `/api` goes to `backend/index.wasm` over the WASM HTTP ABI v1.

The backend is the unmodified Go stdlib source of
[`../go-wasm-http`](../go-wasm-http). This example adds no Go code, no ABI
and no runtime capability.

## Layout

```text
manifest.yaml        fullstack manifest (clientDir: dist, wasmBackend.*)
index.html           SPA (vanilla JS, one "Testar API Go" action)
vite.config.mjs      plain config object, base './'
package.json         build script, Vite version pin
```

Derived, never committed: `dist/` (Vite output), `backend/index.wasm` (Go
output) and the ZIP.

## Routes

- `/api/inspect` and any subpath: JSON echo of method, URI, path, query, host,
  the `x-example` header and the body (read limit 64 KiB).
- `/api/db`: `503` here, because no database configuration is declared. It is
  not a database proof. The guest sees the path without the prefix (`/inspect`
  and `/db`): the EdgeR strips `apiPrefix` before calling the module.
- Anything else under `/api`: `404`. Any other path serves the SPA.

The SPA calls `new URL('api/inspect', document.baseURI)`: the base is the
`<base href>` the EdgeR injects, so the same build works on the name route,
a pinned version route and a vhost. The response is rendered with
`textContent` only; it reads no secrets and no `localStorage`.

## 1. Build the backend (offline, Go stdlib only)

From `../go-wasm-http`, with a local Go toolchain (no `go.sum`, no network),
writing into this directory's `backend/`:

POSIX:

```sh
mkdir -p ../go-wasm-fullstack/backend
cd ../go-wasm-http
GOOS=wasip1 GOARCH=wasm GOTOOLCHAIN=local GOWORK=off GOPROXY=off \
  go build -trimpath -buildmode=c-shared -ldflags='-s -w' \
  -o ../go-wasm-fullstack/backend/index.wasm .
```

Windows (PowerShell):

```powershell
New-Item -ItemType Directory -Force ..\go-wasm-fullstack\backend | Out-Null
Set-Location ..\go-wasm-http
$env:GOOS='wasip1'; $env:GOARCH='wasm'; $env:GOTOOLCHAIN='local'
$env:GOWORK='off'; $env:GOPROXY='off'
go build -trimpath -buildmode=c-shared -ldflags='-s -w' -o ..\go-wasm-fullstack\backend\index.wasm .
Remove-Item Env:GOOS, Env:GOARCH, Env:GOTOOLCHAIN, Env:GOWORK, Env:GOPROXY
```

The reviewed Go 1.27.3 artifact of this source has SHA-256
`8062d9cbed8f48dd8f167d7805b2f83e30e401c0d3a072c9d82dc5f810f535b0`. A fresh
build is a new build: compare its hash before treating it as that artifact.

## 2. Build the SPA

`package.json` pins `vite` 8.1.4, the version the control-panel UI already
installs. With dependencies installed (`bun install` or `npm install`):

```sh
bun run build      # or: npm run build
```

Offline, without installing anything, run an existing Vite 8.1.4 install on
this directory (use the path of your own install):

```sh
bun /path/to/node_modules/vite/bin/vite.js build
```

The output is `dist/index.html` plus a hashed `dist/assets/index-<hash>.js`.
Asset URLs are relative (`base: './'`).

## 3. Assemble the ZIP

The ZIP root holds exactly `manifest.yaml`, `dist/` and `backend/`:

```sh
7z a -tzip go-wasm-fullstack-1.0.0.zip manifest.yaml dist backend
# or: zip -r go-wasm-fullstack-1.0.0.zip manifest.yaml dist backend
```

Check the listing before use: entries must be `manifest.yaml`, `dist/...` and
`backend/index.wasm` with `/` separators and nothing else.

A fresh build of the same sources with the same Vite and Go toolchains can
still produce a ZIP with a different container hash (timestamps, entry order
and compression are not part of the content), and a Vite or Go rebuild is not
guaranteed to be byte-identical to the reviewed files. The opt-in check below
therefore pins the CONTENT of the four files, not the ZIP bytes. A legitimate
change to any of them needs a rebuild, a review and an update of the pins in
the test. Reproducing the package never requires installing new dependencies.

## Manifest notes

- `wasmBackend.entrypoint: backend/index.wasm`, `wasmBackend.apiPrefix: /api`.
- `visibility: public`, `nameRoute: true`. No hostname is declared.
- The Go module imports `edger_http_v1.fetch` even for `/inspect`. The
  manifest enables `egress` with an empty `allowNet`. This GRANTS general
  HTTPS egress: the guard does not read `allowNet`, so the guest can reach any
  HTTPS destination the guard accepts (default port `[443]`, IP classification,
  revalidated redirects, the guard limits); the empty `allowNet` only means no
  direct network for Deno. The guest in this package never calls the import
  outside `/api/db`. No private CIDRs, egress hosts or SQLD settings are
  declared.
- No `release`, `cron`, `secrets`, `env`, `ssrEntrypoint` or top-level
  `entrypoint`. `timeout: 30s`, `maxBodySize: 4mb`.

## Limits

- No release artifact, deploy flow or install step is described here; this
  example is not production guidance. Upload and serving through the EdgeR
  binary are checked separately, not by this README.
- No database provisioning in this example: it declares no `SQLD_URL` or
  `SQLD_TOKEN`, so `/api/db` is `503`. The managed database exists in the
  product binary (`planning/edger/docs/sqld-managed.md`), but is not exercised by
  this package.
- No SSR: the adapter serves static assets and the WASM API only.

## Real check through the `edger` binary (opt-in)

`crates/edger-orchestrator/tests/wasm_fullstack_binary.rs` has the
`#[ignore]` test `real_go_vite_fullstack_package_through_real_edger_binary`.
It launches the real `edger` binary on a loopback port (Unix only), installs
the ZIP of step 3 through the install API and calls it over TCP. It needs the
absolute path of the ZIP. A missing, unreadable, oversized (over 32 MiB) or
non-matching file is a panic, never a skip. The admission is by content: the
archive must hold exactly `manifest.yaml`, `backend/index.wasm`,
`dist/index.html` and `dist/assets/index-BAhFX9bS.js`, each once, with the
pinned size and SHA-256 (the Go module is the reviewed artifact
`8062d9cb...f535b0`), plus optionally the directory entries `backend/`,
`dist/` and `dist/assets/`. Entry order, timestamps and compression may
differ; extra, unknown, hostile-path, missing or changed entries are refused.
This pins file content only; it does not prove that the HTML works in a
browser.

```sh
EDGER_GO_WASM_FULLSTACK_ZIP=/abs/path/go-wasm-fullstack-1.0.0.zip \
  cargo test --locked -p edger-orchestrator --test wasm_fullstack_binary \
  real_go_vite -- --ignored --test-threads=1
```

The test installs the ZIP unchanged as 1.0.0, and a 2.0.0 test overlay whose
manifest differs ONLY in `version` and an added `hosts` entry for the
reserved name `fullstack-go.fixture.test`. The shipped manifest declares no
host; the overlay exists only to exercise the vhost route. The Go module and
`dist/` bytes are identical in both. It checks the default, `@1.0.0`,
`@2.0.0` and vhost routes: the served `<base href>` (also from a deep SPA
route), the module script and `api/inspect` URLs resolved from it,
`POST api/inspect/...` echoing method, URI, query, header and body, the JSON
`404` for an unknown `/api/...` path, `/apix` as SPA, `/api/db` as `503`
(missing configuration, not a database proof), the `.br`/`.gz` variants of the
1600-byte asset, and that the module and manifest are not served as assets.
It is a CLI and package check, not a browser test.
