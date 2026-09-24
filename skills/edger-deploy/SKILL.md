---
name: edger-deploy
description: "Package and deploy apps and workers to EdgeR: write a manifest.yaml, build the zip, install through cPanel, REST, MCP HTTP or MCP stdio, then verify the public route and manage versions (staged, promote, force, delete). Use when asked to deploy, publish or install an app or worker on EdgeR, to write or fix a manifest.yaml, to package a deploy zip, when a static SPA loads blank or assets fail behind a URL prefix, or to install and manage a worker through the EdgeR MCP."
metadata:
  short-description: Package and deploy apps to EdgeR
---

# edger-deploy

Deploy an app or worker to EdgeR without re-reading the code: pick a kind,
write `manifest.yaml`, package a zip, install it through one of four channels
(cPanel, REST, MCP HTTP, MCP stdio), verify the public route, then update via
new versions plus promote.

Examples use `$EDGER_URL` (the control-plane base URL; when the runtime sits
behind a prefix-stripping proxy it ends with that prefix, for example
`https://<host>/apps`) and `$EDGER_API_KEY` (the root key, or a scoped
`egk_` API key with the needed permissions). The worker data plane
`/<name>/` is open: browsing a public worker needs no credential.

## Flow

1. **Pick the kind.**
   - `static` / `spa`: a prebuilt site; served by pure Rust, no Deno process.
   - `fetch` / `serverless` (or a JS/TS `entrypoint`): a worker run in a
     persistent Deno process; entrypoint conventions below.
   - `fullstack` / `ssr` plus `adapter`: a framework SSR build; see
     [references/manifest.md](references/manifest.md).
2. **Write `manifest.yaml`** at the zip root. Minimum for a static SPA:
   `name`, `version`, `kind: static`, `entrypoint: index.html`. Defaults and
   every deploy-relevant field:
   [references/manifest.md](references/manifest.md).
3. **Package the zip.** `manifest.yaml` and `index.html` at the zip root,
   plus the assets. A single top-level folder is unwrapped. Limits: 64 MiB
   compressed, 256 MiB expanded, 50,000 entries; absolute or `..` paths are
   rejected. For a Vite app: `base: './'` in the Vite config, `<base href="/" />`
   in `index.html`, and the manifest in `public/manifest.yaml`. The full
   static-SPA contract: [references/spa-contract.md](references/spa-contract.md).
4. **Pick a channel and install.** All four land in the same admin API:
   - cPanel: the "Deploy an app" dialog; choose the zip (64 MiB cap).
   - REST: `POST $EDGER_URL/api/admin/workers/install` with the raw zip body.
   - MCP HTTP: `edger.install_worker` with `zipBase64` on `$EDGER_URL/api/mcp`.
   - MCP stdio: the `edger-mcp` binary with `zipPath` inside the workspace.
   Commands, headers and arguments:
   [references/deploy-channels.md](references/deploy-channels.md).
5. **Verify.**
   - `curl -sS "$EDGER_URL/<name>/"` returns the `index.html` with
     `<base href="<prefix>/<name>/" />` (with the version segment when the URL
     is `/<name>@<version>/`).
   - Unknown paths return the SPA shell (fallback), not a 404; a missing
     asset surfaces as a MIME error (`text/html`), not a 404.
6. **Update.** Public versions are immutable: bump `version`, repackage,
   install the new version. Zero-downtime: install with `staged=true`, then
   `POST $EDGER_URL/api/admin/workers/<name>/promote?version=<new>`.

## JS/TS worker entrypoints

A JS/TS worker directory needs `index.{ts,js,mjs}` compatible with one of:

- `Deno.serve(handlerOrOptions)`
- `export default { fetch(req) {} }`
- `export default fetchFn`
- `export default { routes }` — Bun.serve-style table: exact match > `:param`
  > `*` wildcard, per-method maps, `fetch` fallback

## Common errors

| HTTP | Code | Cause | Fix |
|---|---|---|---|
| 409 | `COLLISION` | Same `name@version` already installed | Bump `version` and install the new zip |
| 409 | `DEPLOY_PUBLIC_VERSION_IMMUTABLE` | `force` on a public version | Install a new public version and promote it |
| 409 | `DEPLOY_REVISION_REQUIRED` / `DEPLOY_REVISION_STALE` | `force` without — or with a stale — `x-edger-expected-revision` | Send the `revision` returned by the last install or list |
| 400 | `DEPLOY_STAGED_REQUIRES_PUBLIC` | `staged=true` on an `internal` worker | `staged` only applies to `public` workers |
| 400 | `DEPLOY_INVALID_PACKAGE` | No inferable `name`, or no entrypoint | Set `name` and `entrypoint` in `manifest.yaml` |
| 409 | `CORE_WORKER_IMMUTABLE` | Deleting a bundled or overlay version | Only versions installed from user zips can be deleted |
| 404 | `NOT_FOUND` | Unknown name/version, or delete/promote/enable/disable on a worker the key's scope hides | Check `GET $EDGER_URL/api/admin/workers`; out-of-scope workers 404 (hidden) on those operations, not 403 |
| 403 | `FORBIDDEN` | Key lacks the permission, or install of a worker outside the key's `workers` / `namespaces` scope | Grant `workers:install` / `workers:promote` / `workers:delete`, or use a key whose scope covers the worker name |
| 401 | `UNAUTHORIZED` | Missing or invalid credential | Send `Authorization: Bearer $EDGER_API_KEY` |

## SPA blank page or wrong asset paths behind a prefix?

Before touching deploy settings, re-check the SPA contract: only relative
asset URLs (`base: './'` in Vite, no absolute `/...` URLs for the app's own
files), a router basename derived from `document.baseURI`, and assets using
recognized extensions (unknown ones are served as `application/octet-stream`).
Checklist and per-framework basename recipes:
[references/spa-contract.md](references/spa-contract.md).

## References

- [references/manifest.md](references/manifest.md) — `manifest.yaml` fields
  with defaults, name rules and the name fallback order, worker entrypoints.
- [references/spa-contract.md](references/spa-contract.md) — the static-SPA
  contract: `<base href>` rewriting, SPA fallback, `publicEnv`, cache and
  Content-Type, basename recipes.
- [references/deploy-channels.md](references/deploy-channels.md) — cPanel,
  REST, MCP HTTP and MCP stdio; API-key permissions; versions, staged +
  promote, force and delete.
