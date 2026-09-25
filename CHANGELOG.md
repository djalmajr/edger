# Changelog

All notable changes to EdgeR will be documented here.

## [Unreleased]

Toward 0.3.2. Release candidates: `v0.3.2-rc.3` (tanstack public files,
type-only bundle deps); `v0.3.2-rc.2` (owned domains, metrics key,
core-worker precedence); `v0.3.2-rc.1` (published and validated on
labdev: zero-downtime host switch, 21/21 probes 200 across four promotes).

### Changed

- `hosts:` belongs to the worker **name**, not to a version: the host is
  answered by the version the name serves at the moment (the promoted
  default, or the highest enabled non-staged version when there is none), as
  long as that version's manifest declares the host. Versions of the same
  name may repeat the host; a different name declaring an already-claimed
  host is refused with `409 COLLISION`. The new version therefore deploys
  without downtime: install it with `staged=true`, test it at
  `/<name>@<version>/`, then promote it; rollback is promoting the previous
  version. Before, a second version with the same host answered `409`, so
  the deploy required deleting the current one first.
- An owned host is the app's entirely: when a host is claimed through
  `hosts:`, every path on it — `/`, `/health`, `/ready`, `/metrics`,
  `/api/*` and `/.well-known/*` — is answered by the owning worker, and the
  control plane (Admin API, MCP, metrics, health and the `/` redirect) only
  answers on hosts without an owner. An owned host with no version serving
  it answers `404`. The effective authority is the URI authority when
  present (HTTP/2 `:authority` or absolute-form request target, without
  userinfo), otherwise the `Host` header.
- `/metrics` and `/metrics/stats` now require a credential with the
  `observability:read` permission (or the root key): `401` without a
  credential and `403` without the permission. Breaking for scrapers that
  called `/metrics` without a key; with no `ROOT_API_KEY` set (open mode)
  the endpoints stay open.
- Core workers: the cPanel and WebIDE versions now follow the release (the
  publish job checks that the `workers/core/*` manifests match the tag, as
  it already does for `Chart.yaml`); when the bundled and the overlay carry
  the same `name@version`, the bundled version wins and the overlay entry is
  ignored with a log warning; the active cPanel version at boot is the
  persisted default pointer when valid, otherwise the highest semver among
  enabled non-staged versions. The default-version pointer of a core worker
  is written to the writable overlay root.
- cPanel installs through the Admin API follow the active-version rule: a
  cPanel older than the active one answers `activation: "inactive"` and the
  active version keeps serving until a promote; installing with
  `staged=true` does not touch the active version, and the promote is what
  switches it. An explicit enable still activates the requested version.
- TanStack Start (React and Solid): the router basepath is baked into the
  build (Vite `base` and `router.basepath`, burned into the bundle), so one
  build serves exactly one base. With `basePath: auto`, a host route answers
  at `/` and a name route at `/<name>`; serving the same build at both bases
  requires the app to resolve the base at runtime (asset URLs, router
  basepath, server-function base, auth base).

### Added

- `EDGER_BIND` environment variable: the listening IP of the HTTP server
  (IPv4 or IPv6), default `0.0.0.0`. An invalid value fails the start with a
  clear message; the port stays in `PORT`.

### Fixed

- HTTP/2 and `Host: x.:443` no longer escape to the control plane on an
  owned host: the routing authority now comes from the URI when present
  (HTTP/2 `:authority` or absolute-form request target, without userinfo),
  falling back to the `Host` header, and the port is removed before the DNS
  trailing dot, so `x.example.:443` matches the registered `x.example`
  owner.
- Promoting a core version that only exists in the bundled root no longer
  fails with `DEPLOY_IO`: the default-version pointer is written to the
  writable overlay root (the bundled root is read-only in the image).
- Boot no longer fails when the bundled and the overlay carry the same core
  worker `name@version`: the bundled version wins and the overlay entry is
  ignored with a warning.
- Installing a newer cPanel with `staged=true` no longer disables the active
  version, which left the cPanel out of the air until the promote.
- The `tanstack` fullstack adapter now serves any existing public file from
  `clientDir` even when the path matches no `assetPrefixes`. Not served
  through that path: path components starting with `.` (a leading
  `.well-known` is the exception), the TanStack server routes `/api` and
  `/_serverFn` (also when percent-encoded), and paths with no file, which
  fall back to the SSR.
- Type-only imports pointing outside the worker (JSDoc `@type {import(...)}`
  or `import type`) no longer fail the deploy with
  `DENO_BUNDLE_GRAPH_DENIED`: the bundle graph validator now ignores modules
  reachable only through type edges of `deno info --json` (redirects
  included). Code imports to files outside the worker directory are still
  refused.

### Dependencies

- `tokio` 1.53.1, `bytes` 1.12.1, `thiserror` 2.0.21 and `futures-core`
  0.3.34.
- `docker/login-action` v4 and `docker/metadata-action` v6 in the release
  workflow.

## [0.3.1] - 2026-09-25

Validated on labdev as `0.3.1-rc.7`. The `0.3.1-rc.4` upgrade from 0.3.0 ran
through the Rancher UI, `0.3.1-rc.6` and `0.3.1-rc.7` came in through
`helm upgrade`, and the `0.3.1-rc.7` form was checked in the Rancher UI.
Release candidates: `v0.3.1-rc.1` was tagged but never published (two
advisories disclosed after 0.3.0 failed the `cargo deny` gate that guards the
release job); `v0.3.1-rc.2` published its image, but the chart push failed on
Helm's "Tag" step against ghcr (helm/helm#31223); `v0.3.1-rc.3` was the first
complete publish; `v0.3.1-rc.4` added the Rancher form changes;
`v0.3.1-rc.5` added the cPanel actions and file permissions;
`v0.3.1-rc.6` made `Ingress Class` a select and `Ingress Annotations` a
`questionMap`, which Rancher 2.13 does not render (the dashboard lowercases
the type before looking up the component, so the field fell back to plain
text); `v0.3.1-rc.7` moved the annotations to `map[string]`.

### Added

- Agent skill `skills/edger-deploy` (install with
  `bunx skills add djalmajr/edger --skill edger-deploy`): `manifest.yaml`, zip
  layout, static-SPA contract, deploy channels (cPanel, REST, MCP HTTP, MCP
  stdio), versions, staged promotes and deletion.
- The chart README documents installing and upgrading through the Rancher UI
  (OCI repository, form fields, labdev values) and through Helm on the
  terminal.
- API-key permissions `workers:toggle`, `files:read`, `files:write` and
  `files:delete` join the catalog (11 entries; the order is a contract — the
  key migration and the cPanel mirror it).
- `POST /api/admin/workers/{name}/files/delete`: batch deletion of files and
  directories inside a deployed user version — body
  `{"paths": ["a.txt", "dir/sub"]}` (1 to 1000 paths), per-item failures in
  the `200` body (`{"deleted", "errors", "revision", "entries"}`, where
  `entries` is the version's root listing after the operation). The route
  never follows a symlink (a link is removed as the link), refuses the
  version root and the reserved `.edger-revision` file per item, and
  advances the revision and recycles the worker only when at least one item
  was removed. `files:delete` is never granted automatically.
- cPanel: "Set as default" (promote, applied immediately) and "Delete
  version" actions on the workers view — the delete behind a confirmation
  dialog that warns when the version is the default or the only one; file
  deletion in the Files tab — one at a time and in batch via row checkboxes,
  with a confirmation dialog and per-item errors shown inline; and every
  action is rendered only when the logged-in key holds the matching
  permission.

### Changed (chart)

- The Rancher form (`questions.yaml`) now covers an Ingress like labdev's:
  `Ingress Path Type`, `Ingress Class` (a select over the cluster's
  `IngressClass` resources, where empty keeps the cluster default),
  `Ingress Annotations` (a key/value editor), a host that accepts wildcards,
  and `Root Key Secret` / `Root Key Secret Field` to use an existing Secret
  instead of typing the key.
- `ingress.annotations` and `extraEnv` accept YAML text as well as a map/list
  (Rancher sends multiline fields as strings); invalid YAML fails the render
  with an explicit message. Previously the form's default `extraEnv` (`"[]"`)
  was written verbatim into the container `env`.

### Fixed (release)

- The publish job pins Helm to v3.18.6 (later versions fail to tag OCI charts
  on ghcr) and retries `helm push` up to three times to absorb ghcr
  propagation delays.

### Security

- `rustls` 0.23.41 → 0.23.45 (RUSTSEC-2026-0285: TLS 1.3 handshake messages
  accepted across encryption-level boundaries), pulling `aws-lc-rs` 1.18.1,
  `aws-lc-sys` 0.45.0 and `rustls-webpki` 0.103.15.
- `wasmtime`/`wasmtime-wasi` 36.0.13 → 36.0.15 (RUSTSEC-2026-0269: filesystem
  sandbox escape through paths or symlinks with trailing slashes), with the
  matching `cranelift`/`cap-std` patch releases.

### Fixed

- A worker addressed with its version (`/name@1.2.3/...`, `/@scope/name@1.2.3/...`)
  got `/` as its public base: the `<base href>` injected into a static SPA and
  the `x-base` header lost the address segment, so every relative asset
  resolved outside the worker and the page loaded blank. The base now keeps
  the version segment (`<base href="/name@1.2.3/" />`), also behind
  `X-Forwarded-Prefix` (`/apps/name@1.2.3/`).
- cPanel behind a proxy prefix (`/apps/cpanel/`): menu navigation dropped
  the prefix from the URL and a refresh under the prefix fell back to the
  overview. Routes now derive from the runtime-injected `<base href>` (also
  for versioned addresses like `/cpanel@x.y.z/`), so navigation and refresh
  keep the prefix.
- The cPanel deploy dialog's drop zone used to open or download the dropped
  file; it now stages the dropped `.zip` (any other file is rejected) and the
  dialog cancels the browser's default drop.
- A fullstack `tanstack` or `sveltekit` worker with base path `/` (fixed, or
  resolved by `auto`) got `//` instead of `/` on its root, so the router saw
  a doubled-slash pathname; the root now keeps a single `/`.

### Changed

- The release workflow now also publishes the Helm chart as an OCI artifact at
  `oci://ghcr.io/djalmajr/charts/edger`, with the digest of the image built
  for the same tag recorded in `image.digest`. Installing a chart version
  therefore runs exactly that release's image without passing a digest by
  hand. The job fails before publishing anything when `Chart.yaml`
  `version`/`appVersion` differ from the tag.
- `values-labdev.yaml` no longer declares an empty `image.digest` (it would
  override the digest carried by the published chart), and its install
  command, like the chart README, now points at the ghcr chart.
- Behavior change for existing API users — the permission each route
  requires moved to the new granular set: listing and downloading worker
  files now requires `files:read` (was `workers:read`); uploading files now
  requires `files:write` (was `workers:install`) and no longer requires an
  `internal` version — any user-origin version accepts uploads (core stays
  read-only; `DEPLOY_PUBLIC_VERSION_IMMUTABLE` no longer applies to this
  route); enabling/disabling a version now requires `workers:toggle` (was
  `workers:promote`; `promote` itself still requires `workers:promote`).
- One-time migration of existing API keys on store open
  (`PRAGMA user_version`): a key holding `workers:read` gains `files:read`,
  `workers:install` gains `files:write` and `workers:promote` gains
  `workers:toggle`, so the routes above keep working with pre-existing keys;
  `files:delete` is never added. Idempotent: reopening the store changes
  nothing.

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
