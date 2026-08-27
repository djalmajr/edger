# Changelog

All notable changes to EdgeR will be documented here.

## [0.3.0] - 2026-08-27

### Added

- `/api/mcp`: the control-plane MCP over HTTP — POST-only stateless JSON-RPC
  (native batch; tool failures come back as `isError` results, carrying
  `_meta.status` whenever the failure has an HTTP status behind it).
  Tools self-dispatch through the Admin API router with the caller's own
  credential, so permissions, CSRF, worker scope and deploy contracts are
  identical to REST. Remote subset only: no local filesystem/authoring tools,
  and install takes `zipBase64` (96 MiB body limit).
- Persistent API keys with permissions: SQLite store (`EDGER_API_KEYS_DB`,
  default `<workerDirs>/.edger/api-keys.db` on the existing PVC), `egk_`
  prefixed secrets hashed with the historical `edger-auth-v1` salt, per-key
  permission catalog (`workers:read|install|delete|promote|invoke`,
  `observability:read`, `keys:manage`), tenant `namespaces` and a new
  per-worker resource scope (`workers`: exact name or suffix glob). Auth
  order: root key, then `egk_` store, then OIDC.
- Key management everywhere: REST (`GET/POST /api/admin/keys`,
  `POST /api/admin/keys/{id}/revoke`, `DELETE /api/admin/keys/{id}` — 201
  returns the raw key ONCE; revoke is terminal; delete requires prior revoke),
  MCP tools (`edger.list_api_keys`/`create_api_key`/`revoke_api_key` on both
  transports) and a cPanel screen (scope checkboxes, one-time secret panel,
  revoke/delete) gated by `keys:manage`. Anti-escalation everywhere: a
  non-root creator only grants a subset of its own permissions/scopes, with
  no glob subsumption.

- Docs caught up with the code, including debt that predates this release:
  `04-seguranca-e-isolamento` still described an `AuthGate` and `publicRoutes`
  that no longer exist (the data plane has been open since Epic 17) and
  credited API keys to the deleted `edger-ext-auth` crate; `06-operacao-e-testes`
  documented `EDGER_AUTH_DB`, so its backup runbook copied a file the runtime
  never writes. Both now describe the real auth order, the permission catalog,
  both scopes and the anti-escalation rule; `03-contratos-http-e-workers` gained
  the REST and `/api/mcp` contracts; ADR 0006 records why the MCP vocabulary
  moved into `edger-core` and why HTTP tools self-dispatch through the Admin
  router. `planning/edger/scripts/api-keys-mcp-e2e.py` is a re-runnable
  end-to-end gate against a live instance (22 checks).

### Fixed

- Observability no longer hands a scoped key the whole store. Moving `events`,
  `series` and the SSE stream off root-only (above) made
  `observability:read` enough to read every worker's events — worker name,
  namespace and message included — because the only `worker=`/`namespace=`
  filters were the ones the caller chose. The query now also carries the
  principal, and the single `event_matches` predicate the three routes share
  drops anything outside the key's scope. Events that name no worker and no
  namespace belong to the runtime, not to a tenant, and stay visible.
- Per-worker scope no longer leaks through the error feed.
  `GET /api/admin/workers/{name}/errors` read straight from the raw name, so a
  key scoped to one worker got `200` with another worker's recent messages and
  stack traces — the single route by name that skipped the choke point every
  sibling goes through. It now resolves the worker first and answers `404` for
  anything outside the scope, like the rest.
- The aggregate error summary is scoped too. `GET /api/admin/workers/error-summary`
  takes no worker name, so it never met the choke point and returned the whole
  map: every worker that has failed, plus each one's latest message. A non-root
  principal now sees only its own slice.

  All three leaks were found by auditing the new documentation against the
  code, reproduced against a live instance, and pinned by regression tests that
  fail without the fix. They share one shape — a route that reads by name, or
  returns an aggregate, without going through the scope filter — which is now
  spelled out in `docs/developers/04-seguranca-e-isolamento.adoc`.

### Changed

- Observability endpoints (`events`, `series`, `events/stream`) moved from
  root-only to the `observability:read` permission; worker enable/disable
  moved from root-only to `workers:promote` (same which-versions-serve-traffic
  family). Root keeps everything — the change is purely additive for keys.
- BREAKING (stdio MCP): tool failures now come back as `isError` results
  instead of JSON-RPC `-32603` errors, unifying the failure contract with
  `/api/mcp`. JSON-RPC errors remain for parse/unknown-method/unknown-tool.

## [0.2.4] - 2026-08-26

### Fixed

- Digit-less Vite asset hashes (e.g. `index-DgsWFCcn.js`) are now recognized
  as fingerprints and pinned immutable — case mixing beyond a leading capital
  marks a hash; plain words never qualify.

## [0.2.3] - 2026-08-26

### Fixed

- The root redirect (`/` → cPanel) uses a RELATIVE Location: behind a
  stripping proxy the browser now resolves it inside the public prefix
  instead of escaping to whatever owns `/cpanel/` on the shared host.

### Changed

- StaticSpa responses carry a cache policy: HTML is `no-cache` (a stale SPA
  shell kept old code running across deploys), Vite-shaped fingerprinted
  assets (`assets/name-<hash>`) are immutable for a year, and everything
  else revalidates after five minutes. The cPanel and WebIDE builds now emit
  fingerprinted filenames under `assets/` to match.

## [0.2.2] - 2026-08-26

### Fixed

- The cPanel and WebIDE are proxy-prefix-aware: router basepath, admin/metrics
  calls and worker links now derive from the runtime-injected `<base href>`
  instead of hardcoded absolute paths — behind a stripping proxy the SPA no
  longer escapes its prefix (logins hit whatever owned `/api` out there and
  navigation rewrote URLs out of the mount).

## [0.2.1] - 2026-08-25

### Added

- The dispatch honors `X-Forwarded-Prefix` (charset-checked — the value lands
  inside served HTML) when composing `base_href` and `x-base`, so workers
  behind a stripping proxy (e.g. a Kong route with `strip_path`) emit a
  `<base href>` aligned with the public URL.

### Fixed

- Cross-builds from arm64 hosts: the frontend stage runs on the build
  platform (static output; the x86_64 bun requires AVX2 and dies under
  emulation) and `Dockerfile.cross` cross-compiles the orchestrator with the
  cross GCC as linker instead of emulating rustc, which segfaults under
  Rosetta.
- The labdev chart overlay routes public workers through Kong on the shared
  wildcard host (`/p-` as a string prefix, no strip): the rke2 ingress-nginx
  normalizes every pathType to segment semantics, so a string prefix never
  matches there; the Admin API has no public route at all. The image now
  comes from the public ghcr (SemVer tag, digest-pinned), like tenancit.

## [0.2.0] - 2026-08-25

### Changed

- Future releases adopt the O'Saasy License and are classified as source
  available. Copies previously received under MIT retain the rights granted by
  those distributions.
- The Community/commercial boundary is documented without reintroducing a
  generic plugin runtime.

### Added

- Local-first worker observability in the cPanel: bounded operational events,
  logs, live tail, passive health, request correlation and process lifecycle.
- Optional OTLP traces/logs export with W3C context propagation and
  Helm/Rancher configuration, without making a Collector a runtime dependency.
- Version-scoped worker workspace for files, observability and logs.

### Security

- Upgraded Wasmtime and WASI to a patched release line after the public
  dependency audit identified advisories affecting the previous runtime.
- Bounded and redacted worker console capture.
- Manual/on-deploy health checks without periodic polling that would keep
  serverless workers warm.
