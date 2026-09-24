# manifest.yaml reference

`manifest.yaml` (or `manifest.yml`) at the root of the worker directory or zip.
When no manifest file exists, the loader falls back to `package.json`, then to
the conventional entrypoints `index.html`, `index.ts`, `index.js`,
`index.mjs`, `index.wasm`, `index.wat`.

## Deploy-relevant fields

| Field | Default | Effect |
|---|---|---|
| `name` | fallback chain below | Worker identity; simple (`my-spa`) or namespaced (`@scope/my-spa`). Must be non-empty after resolution; `cpanel` and `webide` are reserved. |
| `version` | `latest` | Must be SemVer (or `latest`): unversioned route resolution parses every candidate version, so a non-SemVer version fails `/<name>/` with `PARSE_ERROR` instead of falling back to another. `name@version` is the deploy slot. Public versions are immutable; reinstalling the same slot answers 409 `COLLISION`. |
| `kind` | inferred | `static`/`spa` → StaticSpa; `fetch`/`serverless` → FetchHandler; `routes`/`backend` → RoutesTable; `wasm` → WasmModule; `ssr`/`fullstack` → Fullstack. Inferred when absent: `entrypoint` ending in `.html` → StaticSpa, `.wasm`/`.wat` → WasmModule, otherwise FetchHandler. |
| `entrypoint` | first existing of `index.html`, `index.ts`, `index.js`, `index.mjs`, `index.wasm`, `index.wat` | Executable file; `index.html` for static SPAs. Install only checks that an entrypoint is declared or inferable (explicit `entrypoint` / `ssrEntrypoint`, or a conventional `index.{html,ts,js,mjs,wasm,wat}`) and fails with `DEPLOY_INVALID_PACKAGE` when none is; it does not check that an explicit entrypoint's file exists — `entrypoint: missing.html` installs and only fails when the route is served (`SPA_ENTRYPOINT_INVALID` for a static SPA). Verify the file exists before zipping. |
| `visibility` | `public` | `public`: routable on the open data plane at `/<name>/`. `internal`: reachable only through authenticated control-plane dispatch; the data plane answers 404. |
| `enabled` | `true` | `enabled: false` is rejected on install (`DEPLOY_INVALID_PACKAGE`); the installer enables the version it installs. |
| `injectBase` | `true` | Rewrites `<base href>` in the served `index.html` (static SPAs); see [spa-contract.md](spa-contract.md). |
| `env` | none | Environment for the worker process (JS/TS); never sent to the browser. |
| `publicEnv` | none | Keys from `env` injected into the HTML as `window.__env__`; keys that look like secrets are dropped. |
| `hosts` | none | Exact `Host` aliases routed to this worker (vhost mapping). |
| `basePath` | `auto` | Base path for fullstack/SSR builds (`auto` or a fixed path such as `/base`). Fullstack only. |
| `ssrEntrypoint` / `adapter` | none | SSR/fullstack: the SSR module and the framework adapter (`astro`, `fresh`, `hono`, `lume`, `nextjs`, `nuxt`, `remix`, `solidstart`, `sveltekit`, `tanstack`). Details: `docs/developers/02-modelo-de-dominio-e-manifests.adoc`. |
| `allowNet` | none | Egress allowlist for persistent Deno workers (`--allow-net=host1,host2`). An empty list denies network; when absent, the runtime falls back to `EDGER_DENO_ALLOW_NET`, and with that also absent, network stays open. |
| `maxBodySize` | `4mb` | Cap on request bodies forwarded to the worker; size strings like `10mb` are parsed to bytes. |

Other fields exist (lifecycle `ttl` / `timeout` / `maxRequests`, resources,
`healthCheck`, `cron`, `base` for micro-frontend plugins); they are not needed
for a deploy and are documented in
`docs/developers/02-modelo-de-dominio-e-manifests.adoc`.

## Name rules

- **Reserved names.** `cpanel` and `webide` are core apps: a user worker with
  those names, or with `base: /cpanel` / `/webide`, is rejected with
  `CORE_NAME_RESERVED`.
- **Reserved paths.** `/health` and `/ready` (exact), `/api` and `/api/*`, and
  `/.well-known*` are runtime routes. A worker named `api` or `health` is
  installed but unreachable at those paths.
- **Name fallback order.** `name` in the manifest, then `name` in
  `package.json`, then the `x-edger-package-name` header, then the
  top-level folder of the zip. If none yields a name, install fails with
  `DEPLOY_INVALID_PACKAGE`.
- **Version fallback.** `version` in the manifest, then `version` in
  `package.json`, then the literal `latest`.
- **Disk sanitization.** The on-disk name keeps only `[A-Za-z0-9._-]`
  (leading `@` stripped, other characters become `-`); a name that sanitizes
  to empty fails with `DEPLOY_INVALID_PACKAGE`.

## Minimal static-SPA manifest

```yaml
name: my-spa
version: "1.0.0"
kind: static
entrypoint: index.html
injectBase: true
env:
  APP_GREETING: "Hello from the manifest"
publicEnv:
  - APP_GREETING
```

## JS/TS worker entrypoints

JS/TS workers run in a persistent, sandboxed Deno process (read limited to
the worker directory; write and run denied; network per `allowNet` /
`EDGER_DENO_ALLOW_NET`). The entrypoint must be compatible with one of:

- `Deno.serve(handlerOrOptions)`
- `export default { fetch(req) {} }`
- `export default fetchFn`
- `export default { routes }` — Bun.serve-style route table: exact match >
  `:param` > `*` wildcard, per-method maps, `fetch` fallback
