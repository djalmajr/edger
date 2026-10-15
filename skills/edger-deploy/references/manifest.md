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
| `release` | none | Command run once per deployed version before the worker serves (migrations, etc.). **Root only (EDG-24):** a non-root install of a package with a non-empty `release` (Admin API, MCP `edger.install_worker`, draft channel) and a non-root files upload writing the version-root `manifest.yaml`/`manifest.yml` with a non-empty `release` are refused with `403 RELEASE_REQUIRES_ROOT` before any write (the root is decided by the canonical destination — `path=.` and any other alias of the root are covered — and the refusal is monotonic over every root manifest entry, both names, any order). Upload manifest inspection has an aggregate 1 MiB budget across ALL root manifests — the sum of the declared sizes is checked before reading any entry, and the sum of the bytes actually read is enforced too — and an over-budget upload is refused with `413 PAYLOAD_TOO_LARGE`. Reserved and manifest names are compared by Unicode case folding (double fold, upper then lower): `MANIFEST.YAML`/`Manifest.yml` count as the version-root manifest and `.EDGER-RELEASE` — as well as Unicode aliases such as `manife\u{17F}t.yaml` (long s) and `.edger-relea\u{17F}e` — as the reserved names, on any filesystem; a non-root upload never writes a Unicode alias of the manifest name at the version root, even without `release`. The intended production target is Linux (case-sensitive filesystem), where the rule is conservative. It runs OUTSIDE the Deno sandbox as a subprocess of the EdgeR process with a clean environment (only `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL` plus the manifest `env`) — it never sees `ROOT_API_KEY` or other workers' secrets. The `.edger-release` marker is reserved EdgeR state: the files delete answers the batch with a per-path error `code: FORBIDDEN`, message `"reserved file"` (HTTP 200) and uploads skip a zip-root entry named like it (including via root path aliases). A `.edger-release` shipped inside a deployed package — including under the single wrapper folder that the installer unwraps — is purged before the version routes, so `release: completed` only reports a command that really ran. |
| `injectBase` | `true` | Rewrites `<base href>` in the served `index.html` (static SPAs); see [spa-contract.md](spa-contract.md). |
| `env` | none | Environment for the worker process (JS/TS); never sent to the browser. |
| `publicEnv` | none | Keys from `env` injected into the HTML as `window.__env__`; keys that look like secrets are dropped. |
| `hosts` | none | Exact `Host` aliases (vhost mapping). The aliases belong to the worker's **name**, not to one version: the host is answered by the version the name serves at the moment (the promoted default, or the highest enabled non-staged version when there is none) as long as that version's manifest declares the host; versions of the same name may repeat the host. A different name declaring an already-claimed host is refused with `409 COLLISION`. |
| `basePath` | `auto` | Base path for fullstack/SSR builds (`auto` or a fixed path such as `/base`). Fullstack only. TanStack Start note: the basepath is fixed at build time (Vite `base` + `router.basepath`, burned into the bundle), so one build serves one base — with `auto`, a host route answers at `/` and a name route at `/<name>`; serving the same build at both bases requires the app to supply a runtime base, because the EdgeR does not rewrite SSR HTML. |
| `assetPrefixes` | per-adapter list | Fullstack static-asset prefixes served from `clientDir`. Defaults: tanstack — `/assets/`, `/favicon.ico`, `/logo192.png`, `/logo512.png`, `/manifest.json`, `/robots.txt`; astro — `/_astro/`, `/favicon.ico`, `/robots.txt`; nuxt — `/_nuxt/`, `/favicon.ico`, `/robots.txt`; solidstart — `/_build/`, `/favicon.ico`, `/robots.txt`; remix — `/assets/`, `/favicon.ico`, `/robots.txt`; hono — `/assets/`, `/favicon.ico`, `/manifest.json`, `/robots.txt`; lume — `/`. Declaring it replaces the adapter default (an explicitly empty list falls back to the default), and a list that resolves to empty disables static serving entirely, including the rule below. On `tanstack`, a path matching no prefix is still served from `clientDir` when the file exists inside the root; it falls back to the SSR when a path component starts with `.` (a leading `.well-known` is the exception), on the TanStack server routes `/api` and `/_serverFn` (checked after percent-decoding too), or when no file exists. |
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
  `/.well-known*` are runtime routes on hosts without an owner. A worker named
  `api` or `health` is installed but unreachable at those paths on such hosts.
  On a host declared in `hosts:`, the app receives every path, including these.
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
