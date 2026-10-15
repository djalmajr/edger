# Changelog

All notable changes to EdgeR will be documented here.

## [Unreleased]

Toward 0.3.2. Release candidates: `v0.3.2-rc.19` (root-only worker
release command, cron follows the default version, owner-less public
routes hidden for workers with their own hosts, production platform
manifests, RustSec patches); `v0.3.2-rc.18` (root-only console
sessions and settings, grouped cPanel navigation); `v0.3.2-rc.17` (warmup
operational event, safe-Rust YAML parser, deno bundle retry); `v0.3.2-rc.16` (version
enable/disable survives restarts); `v0.3.2-rc.15` (each worker version
bundled once, off the async runtime); `v0.3.2-rc.14` (per-frame stream
timeout cancels and drains instead of recycling, wasmtime 49, Rust 1.99);
`v0.3.2-rc.13` (opt-in warmup of fresh
processes, total stream duration cap, active requests in `/metrics/stats`,
per-version recycle); `v0.3.2-rc.12` (minProcesses prewarmed on
install/promote/enable and refilled when a removal empties the group);
`v0.3.2-rc.11` (abandoned streams cancelled
in the worker, precompressed assets, configurable compression, per-worker
request metrics, minProcesses kept as a floor); `v0.3.2-rc.10` (dispatch
telemetry visible in production logs); `v0.3.2-rc.9` (abandoned streams drained
instead of killing the worker process); `v0.3.2-rc.8` (worker slot released when
a streamed response is fully produced); `v0.3.2-rc.7` (compressed app responses,
static asset revalidation, wasmtime security patch); `v0.3.2-rc.6` (cPanel
language and API key management, routing flag state); `v0.3.2-rc.5` (tenant routing, weighted
rollout, console authentication and user management); `v0.3.2-rc.4` (Rust
1.98 toolchain, rusqlite 0.40); `v0.3.2-rc.3` (tanstack public files,
type-only bundle deps); `v0.3.2-rc.2` (owned domains, metrics key,
core-worker precedence); `v0.3.2-rc.1` (published and validated on labdev:
zero-downtime host switch, 21/21 probes 200 across four promotes).

### Added

- Production platform manifests and runbooks under `deploy/production/`
  (EDG-25, nothing applied): the `edger` namespace, the chart values
  (`fullnameOverride: edger`, Secret `edger-root`, 1 Gi request and 4 Gi
  limit), a shared `sqld` 0.24.32 with namespaces (admin API on loopback
  behind `LIBSQL_ADMIN_AUTH_KEY`, one Service per database, a
  NetworkPolicy), one backup CronJob per database, a control-plane
  IngressRoute limited to the control-plane paths, the `edger-ensaio`
  rehearsal overlay (DNS-only egress, cron off, throwaway root key) and
  the cutover and rollback runbooks.

- Console sessions and settings API (root only, all `Cache-Control: no-store`):
  `GET /api/admin/sessions` lists up to 1000 non-expired sessions of active
  users ordered by creation then id, with a `truncated` flag, exposing only
  opaque metadata (id, userId, username, role, isRoot, createdAt, expiresAt,
  current) — never a token or hash. `DELETE /api/admin/sessions/{id}` passes
  the origin gate of the other mutating console routes, validates the id as
  32 lowercase hex characters (400 otherwise, 404 unknown) and revokes
  another user's session so its token stops authenticating immediately;
  revoking your own session is a 403 guarded inside the atomic DELETE (logout
  ends your own). `GET /api/admin/settings` is a read-only allowlist of the
  runtime version, the tenant and weighted routing flags, whether password
  login is available, and the existing session, password and login policies —
  no URLs, paths or secrets.
- cPanel console: active-sessions and settings pages (root only, `Sistema`/System
  navigation group; non-root never sees the group and the page queries never
  run). Sessions lists the connected sessions in a fixed-layout grid with a
  case-insensitive search, a 30 s refresh and a confirmation dialog before
  revoking a foreign session — the current session stays blocked and only
  ends through logout. Settings shows the installation's startup values in
  read-only cards (runtime version, routing flags, password and session
  policies) with no write controls. Both pages follow the selected language
  (pt-BR, en-US, es-ES) and keep the proxy/version route prefixes.
  Layout pins: Users and API Keys put the local search on the left and the
  Refresh/Columns/create actions on the right in one wrapping row; the
  Actions column header is visible and right-aligned; Users orders Status
  before Created with an elastic username column, and API Keys keeps the
  elastic Name and Key columns (`elasticColumnIds`, backward compatible with
  `elasticColumnId`). The Columns menu drops its redundant inner label and the
  permission labels inherit the platform's default font size. The navigation
  is grouped into Management, Operations and System.
- Worker manifests may set `streamTimeout` as a duration (including `0` to
  disable it); otherwise `EDGER_STREAM_MAX_DURATION_MS` applies, defaulting to
  five minutes (`0` disables the global default). When a multiprocess streamed
  response reaches its total duration, EdgeR sends the existing cancel frame,
  drains within the configured abandon-drain limits, and reuses the process
  after a clean end or recycles it when draining fails. `/metrics` exposes
  `edger_stream_max_duration_total{outcome=...}`, and the cut emits a
  `stream.max_duration` operational event with the worker, process, request,
  duration and drain outcome. `/metrics/stats` includes each process's
  `activeRequest` (`requestId`, `ageMs`, and `streaming`), including synthetic
  health-check and warmup requests; every process item also exposes its own
  `id` while the worker-group `id` stays compatible. Admins with
  `workers:toggle` can recycle one version's processes through
  `POST /api/admin/workers/{name}/recycle?version=<versão>` without deleting
  the version or changing its activation.
- Opt-in worker `warmup` manifest field (an object with `path` and optional
  `timeout`, default 10 s): right after a process created by the
  `minProcesses` prewarm or the floor replenishment is spawned — before it
  goes `Idle` — the pool sends ONE synthetic `GET` (always GET) to it, so the
  first user request finds the code already executed once. The warmup is not
  counted toward `maxRequests` or the request metrics, a non-2xx/3xx answer
  only warns (the socket is intact), and a warmup dispatch failure only logs:
  the process is terminated and removed without automatic replenishment, and
  it never fails the boot, the rescan or the install.
- Worker warmup operational event: every warmup emits ONE `worker.warmup`
  operational event (level `info` for the `ok` outcome, `warn` for `status`
  — a non-2xx/3xx answer — and `failed` — a dispatch error —) with the
  worker, version, process id, the `warmup-<uuid>` request id, the duration
  and the answered HTTP status when there is one. The warmup success is now
  logged at `info` instead of `debug`, and a failed warmup keeps its
  `warmup_failed` termination (`SpawnFailed` policy) with a single
  `process.terminated` event.
- cPanel column visibility controls for API Keys and Users: permissions are
  hidden by default, choices persist independently per table in the browser,
  and management actions remain visible. API Keys keeps compact trailing
  columns while the name column absorbs extra space.
- cPanel searches API Keys and Users locally, with case-insensitive matching,
  pagination reset and localized empty-search results.
- Log filters support combinations of severity levels, combined with text search.
- The routing policy section stays hidden while both routing feature flags are off.
- Workers alert badges explain their disabled-version and recorded-error
  counts in a localized tooltip anchored to the corner badge, available on hover
  and keyboard focus without affecting the expansion button.
- `EDGER_BIND` environment variable: the listening IP of the HTTP server
  (IPv4 or IPv6), default `0.0.0.0`. An invalid value fails the start with a
  clear message; the port stays in `PORT`.
- `GET /api/admin/state/export` (root only): a consistent, online backup of
  the EdgeR state while the process is up. The response is a zip streamed
  from a temporary file (no 64 MiB download limit) with `user-roots/<i>/`
  (each user worker root, in index order), `core-overlay/` (the core worker
  overlay root, when present), `api-keys.db` (a consistent copy made with
  `VACUUM INTO` on the store's connection, only when a key store is
  configured) and `edger-state.json` (format, EdgeR version, creation date
  and the source paths). Transient deploy files, the raw database file and
  its sidecars, the top-level `.edger/` of the user roots and symlinks are
  excluded. The export waits up to 30 s for in-flight mutations to settle
  and answers `503 STATE_BUSY` when they do not; a mutation attempted while
  an export is running answers `409 STATE_EXPORT_IN_PROGRESS`. Restore is
  offline and documented (stop, extract the zip into the paths recorded in
  `edger-state.json`, start again): `docs/developers/06-operacao-e-testes.adoc`
  and the chart README.
- Tenant routing (opt-in, `EDGER_TENANT_ROUTING_ENABLED`, off by default):
  per-app tenant allowlists keyed by the app's full name, stored under
  `.edger-routing` and changed only by root
  (`PUT`/`DELETE /api/admin/routing-policy?name=<full-name>`). The allowed
  tenant slugs come from Tenancit's `/v1/identify` hostname directory: the
  confirmed slug is domain context, not authentication of the visitor, and
  a restricted app fails closed when the identity is missing, divergent or
  unavailable. With the flag off, no Tenancit URL or token is required and
  the EdgeR never calls it; with the flag on,
  `EDGER_TENANCIT_IDENTIFY_URL` and `EDGER_TENANCIT_TOKEN_FILE` are
  mandatory. The policy file is local to one instance; a multi-replica
  deployment still requires a shared store or control before enabling.
- Weighted version rollout (opt-in, `EDGER_WEIGHTED_ROUTING_ENABLED`, off by
  default): requests without an explicit `@version` are distributed across
  the eligible versions (public, enabled, non-staged) by an opaque session
  cohort cookie, with the selection bucket derived from the cohort and app
  name — the same session keeps the same choice, and weights express the expected distribution
  of sessions, not of every request. An explicit `@version` bypasses the
  split but still passes the tenant gate when it is enabled. With the flag
  off, the cohort cookie is not issued and the current `defaultVersion`
  serves.
- cPanel console sign-in by password or token, driven by the
  `login-options` contract and following the selected language, plus
  administration of console users (root only): create, edit, enable/disable,
  password reset and delete, with the operator's own grants and immediate
  session revocation on disable; the root is seeded when absent. Chart
  configuration can reference an existing Secret for the root password
  (`consoleAuth.rootPasswordSecret`).
- In-place editing of API key permissions (`PATCH /api/admin/keys/{id}`):
  the grants change without replacing the secret. The server validates the
  requested grants against the permission catalog, applies the
  anti-escalation rule, refuses a revoked key, and refreshes authorization
  immediately after an update.

### Changed

- Worker manifests (`manifest.yaml`/`manifest.yml`) are parsed with
  `serde-saphyr` (safe Rust) instead of `serde_yaml`: `unsafe-libyaml` leaves
  the dependency graph and the `RUSTSEC-2024-0449` advisory ignore is
  removed. The five duration fields (`ttl`, `streamTimeout`, `shutdownGrace`,
  `cooldown`, `queueTimeout`) now hold an `edger-core` value type instead of
  a YAML crate type, with the same accepted forms: a non-negative integer is
  seconds, a string is a duration, and a float, bool, null, negative number,
  sequence or mapping is accepted at parse time and ignored (the field's
  default applies). Measured behavior differences versus the old parser:
  duplicate struct fields (e.g. `name` twice) keep the last value instead of
  failing; `ttl: 1_000` (an underscored number) was an ignored string and
  now means 1000 s; `ttl: 0052` (a leading-zero number) used to mean 52 s
  (a string the duration parser accepted) and is now read as the float 52.0,
  which is ignored, so the field's default applies; integers beyond 64-bit
  range parse as float instead of failing; non-finite floats (`.nan`, `.inf`,
  `1e999`) are now a parse error instead of an ignored value; and a duration
  mapping with a non-string key is now a parse error instead of an ignored
  value.
- The clippy gate now covers test targets: `cargo clippy --workspace
  --all-targets -- -D warnings` in CI and the documented gates, with the
  test-target lints it surfaced fixed at the root (no `#[allow(...)]`).
- Worker versions that need bundling are now bundled ONCE per version and
  reused by every later process of that version: the multiproc spawn path
  runs `deno bundle` a single time, publishes the result atomically to
  `<worker_dir>/.edger-bundle-cache/<key>/` (inside the worker's own version
  dir, so it stays under that worker's read permission and disappears with
  the version) and every subsequent spawn reuses it. The cache key is the
  sha256 of the worker files' relpath+size+mtime fingerprint plus the
  `deno --version` output (memoized per process), the bundler arguments and
  the canonical absolute worker path, so editing a source file in place (or
  copying the version dir to a different location) invalidates it; concurrent spawns of the
  same version single-flight on one build (the others wait, bounded by the
  worker `timeout`), a failed build is never cached, and a non-writable
  worker dir keeps the legacy per-process tempdir behavior (logged once at
  `warn`). The fingerprint walk and the bundler run in
  `tokio::task::spawn_blocking` — no `std::process` on an async runtime
  thread in the multiproc spawn path. `/metrics` exposes, for the multiproc
  backend, `edger_worker_bundle_total{outcome="hit|miss|error|uncached"}`
  and each miss logs an `info` line with the worker dir name, key prefix and
  `duration_ms`. The cache dir and the legacy `.edger-bundle-*` / `.edger-bridge-*` tempdirs
  are hidden from the worker file listing and the zip download/export (direct
  paths into them answer 404; other dot files, such as `.well-known/`, stay
  visible).
- A per-frame read timeout on a multiprocess streamed body now cancels and
  drains instead of recycling: when a streamed response stalls past the
  worker `timeout` (the per-frame read timeout), EdgeR sends the existing
  cancel frame, drains within the configured abandon-drain limits, and
  reuses the process after a clean end or recycles it when draining fails —
  the same path as the `streamTimeout` total-duration cut. `/metrics`
  exposes `edger_stream_frame_timeout_total{outcome=...}` (a frame-timeout
  cut never moves `edger_stream_abandoned_total` or
  `edger_stream_max_duration_total`), and the cut emits a
  `stream.frame_timeout` operational event with the worker, process,
  request, stream age at the cut, the per-frame timeout (structured field
  `timeoutMs`) and drain outcome. A disabled drain (a `0` abandon-drain
  limit) or a failed cancel write
  recycles with the `socket_poisoned` sub-cause, as before.
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
- The MSRV is now 1.98 (`rust-version = "1.98"`): compiling locally
  requires `rustup update stable` (1.98.1).
- The promote and the release-marker write in the rescan now take the
  version's mutation slot: a promote concurrent with another mutation of the
  same `name@version` answers `409 DEPLOY_IN_PROGRESS`, and the rescan
  skips, with a log warning, any version whose slot is already held — its
  pending release runs on the next rescan or boot. Before, the promote
  wrote the default pointer and the staged marker without holding the
  version slot, and the rescan ran the release command the same way.
- Helm chart: with worker persistence enabled, the core worker overlay is
  persisted on the workers PVC under `.edger/core-overlays` (new
  `runtime.persistCoreWorkerOverlay`, default `true`), so a core worker
  installed through the Admin API survives pod restarts and chart upgrades;
  before, it lived in an `emptyDir` and was lost on every restart. On image
  upgrades the highest enabled cPanel semver is active unless a version was
  explicitly promoted; a promoted version remains the default.
  `runtime.persistCoreWorkerOverlay: false` keeps the previous `emptyDir`
  behavior.
- cPanel overview: follows the selected language and shortens long worker
  identifiers in the table while preserving the full value on hover.
- The routing policy editor reflects the effective runtime feature flags
  and stays disabled in lab-dev while tenant and weighted routing are off.
- cPanel API keys: the list uses the shared DataGrid with a bounded
  permissions column, the creation action shares the page action row,
  permission badges are limited to two lines with a count and an accessible
  list of hidden permissions, and the page follows the selected language.

### Fixed

- The manifest `cron[]` jobs now follow the default version and track the
  index (EDG-26): instead of a fixed registration list built once at boot,
  the cron scheduler reconciles the desired job set from the `ManifestIndex`
  every 5 s (the interval is injectable in `CronSchedulerConfig`), so
  install, promote, enable, disable and delete take effect within one
  reconcile interval. Only the version the unversioned route `/<name>/`
  resolves to — the promoted default pointer, or the highest enabled,
  non-staged semver when there is no pointer, through the same resolver
  the pipeline uses — contributes its `cron` entries; enabled rollback
  versions and staged versions never dispatch. A job is identified by
  (worker, schedule, path, method) WITHOUT the version, so a promote with
  the same `cron` entries keeps the running task, and each tick dispatches
  the unversioned route resolved at tick time. Every dispatch logs one
  stable info line `cron dispatched` with `worker`, `version` (the plain
  value of the version the unversioned route resolved at that moment, or
  `unresolved` when it cannot be resolved), `path`, `method`, the
  `cron-<uuid>` `request_id` and the response `status` plus `duration_ms`
  (the dispatch duration in milliseconds); the
  missing-`ROOT_API_KEY` warning now fires once, when the desired set
  leaves empty. An unparseable schedule no longer fails the boot: the job
  is skipped and counted once in `edger_cron_failures_total` until the
  manifest changes. Each job runs its dispatches sequentially: a job
  (worker, schedule, path, method) never has more than one dispatch in
  flight — a dispatch slower than the `@every` interval delays the next
  tick instead of overlapping it. A removed job (disable, delete, promote)
  is drained, never aborted: its generation is signaled and stops
  scheduling, an in-flight dispatch finishes normally, and the generation
  stays tracked (visible as `draining_job_count`) until it is reaped —
  reconciliation never waits on a removed task (a slow dispatch cannot
  block the other workers' cron). All job-task state is owned by the
  scheduler's supervisor task alone (no shared maps, no handle
  transfers): every job task is born in the supervisor's `JoinSet` (if
  the supervisor is dropped or aborted, the `JoinSet` drop aborts the
  children, so nothing is detached), and each job key keeps a LIST of its
  live generations — repeated removals only append, the reaping removes
  the finished ones, and a new generation waits for all still-live
  generations of its key before its first dispatch (no overlap across
  repeated removals/re-adds). `active_job_count`/`draining_job_count`
  read values published by the supervisor. Only the global shutdown may
  force-stop a task: the supervisor stops admitting jobs, signals every
  generation (an in-flight dispatch is never aborted mid-flight), lets
  the in-flight dispatches finish until the grace deadline
  (`with_shutdown_grace` on `CronSchedulerConfig`, default 5 s), then
  aborts the stragglers and waits for their end — confirmed through the
  supervisor's own `JoinSet` — until the abort-wait deadline
  (`with_shutdown_abort_wait`, default 1 s). The counters change only
  through the actual reaping (never `store(0)`): after `shutdown` returns,
  `active_job_count() + draining_job_count()` is the number of tasks
  still in the `JoinSet`, and a task that does not yield by the deadlines
  is reported (`cron shutdown incomplete` warn with `remaining=<n>`, the
  counters keep the count) instead of being declared stopped; if the
  supervisor itself stalls past the external limit (grace + abort-wait +
  1 s margin) it is aborted (`cron supervisor did not stop` warn) and its
  `JoinSet` drop repeats the abort request. `EDGER_CRON_ENABLED=false`
  still disables everything.
- Worker bundling retries `deno bundle` once (300 ms) after a transient
  esbuild fetch failure before using the minimal relative-module fallback
  (EDG-14), so a cold cache under load keeps producing the real bundle
  instead of the fallback; the deno bundler test now detects the fallback
  bundle and only asserts the D29 graph property on the real `deno bundle`
  path. The legacy bridge isolate (`EDGER_JS_RUNTIME=bridge`) now runs the
  synchronous CLI runner on a blocking thread, so a bundling retry or a slow
  `deno run` no longer blocks the async runtime thread and the request
  wall-clock limit can fire.
- Per-version enable/disable now persists across restart, rescan and
  state export/import (EDG-21): the operator's choice is stored in a
  `.edger-enabled` marker inside the version dir and re-applied when the
  index loads, where it beats `manifest.yaml` `enabled` in both directions
  (a persisted `enabled=true` keeps a `enabled: false` manifest version up)
  and is applied before the `minProcesses` prewarm, so a disabled version
  stays disabled and its floor is never spawned. The toggle persist claims
  the mutation slot of every indexed version of the worker before planning
  and rolls every marker (and a cleared persisted default pointer) back on
  any later failure, so memory and disk never disagree; a toggle that turns
  off the version the persisted default pointer names clears that pointer.
  The marker is operator state, not package or user content: an install
  discards a shipped marker, the files delete API refuses to delete it
  (reserved), and a files upload skips a zip-root marker entry. The marker
  travels with the version dir through export/import.
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
- The two console sign-in modes (password and token) no longer share the
  button progress state: each button shows progress only for its own
  request.

### Security

- Public owner-less routes hidden for workers with their own hosts (EDG-27):
  on a host without an owner — including the control-plane host — every
  public route that does not arrive on the worker's own hosts (the name
  route `/<name>/…`, with or without an `@version` pin, including staged
  and rollback versions; the homepage fallback; and the plugin `base`
  route) answers `404 NOT_FOUND` in the same shape as an unknown worker when
  the version that will actually receive the dispatch declares `hosts`,
  unless it opts in with `nameRoute: true` in the manifest — the rule is
  decided by the finally selected version, so a weighted-routing selection of
  a protected version is refused, and a protected default does not hide a
  chosen version that opted in. The refusal happens in dispatch preparation,
  after the final version selection and before any dispatch, and does not
  count in the worker's request metrics; the own-host (vhost) routing,
  workers without `hosts` and internal requests (authenticated control-plane
  invoke, cron with root Bearer) are unchanged. Closes the bypasses that
  reached such a worker — around the protections it puts on its own
  domain's ingress, like a Cloudflare `ipAllowList` — through
  `https://<control-plane-host>/<name>/…`, through the homepage fallback and
  the plugin base on an owner-less host, and through a weighted selection of
  a protected version.

- Worker `release` command (EDG-24): only root may declare it — an install
  by a non-root key (`workers:install`) of a package with a non-empty
  `release`, and a non-root files upload (`files:write`) writing the
  version-root `manifest.yaml`/`manifest.yml` with a non-empty `release`,
  are refused with `403 RELEASE_REQUIRES_ROOT` before any write. The root
  is decided by the canonical destination (`path=.` and other root aliases
  are covered), the refusal is monotonic over every root manifest entry
  (both names, any order) and the upload manifest inspection has an
  aggregate 1 MiB budget across all root manifests — the sum of the
  declared sizes is checked before reading any entry and the sum of the
  bytes actually read is enforced too (`413 PAYLOAD_TOO_LARGE` over it).
  Reserved and manifest names are compared by Unicode case folding (double
  fold, upper then lower): `MANIFEST.YAML`/`Manifest.yml` count as the
  version-root manifest and `.EDGER-RELEASE` — including Unicode aliases
  such as `manife\u{17F}t.yaml` and `.edger-relea\u{17F}e` (long s) — as the
  reserved names; a non-root upload never writes a Unicode alias of the
  manifest name at the version root, even without `release`. The intended
  production target is Linux (case-sensitive filesystem), where the rule is
  conservative. The release subprocess now starts from a clean environment
  (only `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL` plus the manifest `env`)
  and no longer inherits `ROOT_API_KEY`, `EDGER_*` or any other variable of
  the EdgeR process environment. `.edger-release` is a reserved
  EdgeR marker: the files delete answers the batch with a per-path error
  `code: FORBIDDEN`, message `"reserved file"` (HTTP 200), uploads skip a
  zip-root entry named like it, and a marker shipped inside a deployed
  package (including under the single wrapper folder and in case/Unicode
  variants) is purged before the version routes, so `release: completed`
  only reports a real run.

### Dependencies

- `rustls` 0.23.46 and `wasmtime`/`wasmtime-wasi` 49.0.3 (lockfile only)
  for RUSTSEC-2026-0346 to 0349 and RUSTSEC-2026-0352 to 0364.

- `wasmtime` and `wasmtime-wasi` 36 -> 49 (49.0.2) and `wat` 1.261.0: the
  WASI preview1 bindings moved from `wasmtime_wasi::preview1` to
  `wasmtime_wasi::p1`; the sandbox capability configuration, the
  epoch watchdog and the memory limits are unchanged.

- Wasmtime and wasmtime-wasi 36.0.16 patch the RustSec advisories
  RUSTSEC-2026-0316 and RUSTSEC-2026-0314 while keeping the current major.

- `tokio` 1.53.1, `bytes` 1.12.1, `thiserror` 2.0.21 and `futures-core`
  0.3.34.
- `hyper` 1.12.0, `uuid` 1.27.0 and `serde_json` 1.0.151.
- `docker/login-action` v4 and `docker/metadata-action` v6 in the release
  workflow.
- `rusqlite` 0.40.2 with the `fallible_uint` feature: the 0.40 line drops
  the `u64` `ToSql`/`FromSql` impls, and the feature restores them as
  fallible conversions, so the key store keeps its `u64` fields. The
  embedded SQLite it ships moves 3.46.0 to 3.53.2 (`libsqlite3-sys` 0.30.1
  to 0.38.2); existing key stores open and migrate — the review probed an
  `api-keys.db` created by the 3.46.0 amalgamation: it opens, keeps
  `journal_mode delete`, preserves the row and `created_at`, and accepts
  the `user_version` migration.
- Rust 1.99 toolchain in the workspace `rust-version`, the `Dockerfile`
  and `Dockerfile.cross` builder images, the three GitHub Actions
  `dtolnay/rust-toolchain` steps, and the GitLab CI job images.
- `oven/bun` 1.4.2 in the frontend build stage of both Dockerfiles.

## [0.3.2-rc.19]

- Only root may declare a worker `release` command: a non-root install or
  files upload that sets it is refused with `403 RELEASE_REQUIRES_ROOT`;
  `.edger-release` is reserved and purged from packages, reserved names
  compare by Unicode case folding and the release runs with a clean
  environment (EDG-24).
- Cron jobs follow the default version and track the index: the
  scheduler reconciles every 5 s and dispatches through the unversioned
  route, so install, promote, enable, disable and delete take effect
  without a restart; removal never aborts an in-flight dispatch and
  shutdown reports tasks that do not yield (EDG-26).
- Workers with their own hosts are not served by owner-less public
  routes (name route, `@version` pin, homepage fallback, plugin base)
  unless the manifest sets `nameRoute: true` (EDG-27).
- Production platform manifests for the `edger` namespace and the shared
  `sqld` under `deploy/production/` (EDG-25, nothing applied).
- `rustls` 0.23.46 and `wasmtime` 49.0.3 for RustSec advisories.

## [0.3.2-rc.18]

- Root-only console sessions and settings: `GET /api/admin/sessions` lists
  opaque session metadata (never a token or hash), `DELETE
  /api/admin/sessions/{id}` revokes another user's session immediately (your
  own session is a 403; logout ends it) and `GET /api/admin/settings` returns
  a read-only allowlist of the runtime version, routing flags and console
  policies. All three answer `Cache-Control: no-store` (EDG-23).
- cPanel: Sessions and Settings pages under a root-only System group, the
  navigation grouped into Management, Operations and System, and the Users /
  API Keys toolbar and column layout pins (EDG-23).

## [0.3.2-rc.17]

- Each opt-in warmup now emits a `worker.warmup` operational event (outcome
  `ok`, `status` or `failed`, with the HTTP status, process id and duration),
  `info` on success and `warn` otherwise, so production can confirm that a
  warmup happened (EDG-19).
- Worker manifests are parsed with `serde-saphyr` (safe Rust) instead of
  `serde_yaml`, so the unmaintained `unsafe-libyaml` (`RUSTSEC-2024-0449`)
  leaves the dependency graph. Real manifests parse to the same values; the
  corner-form differences are listed under Unreleased / Changed (EDG-22).
- `deno bundle` is retried once after a transient esbuild fetch failure
  before the minimal fallback, and the legacy bridge isolate runs the CLI
  runner off the async runtime thread (EDG-14).
- The clippy gate now covers test targets, and the yanked `spin` 0.9.8 is
  updated to 0.9.9.

## [0.3.2-rc.16]

- Enabling or disabling a worker version through the Admin API or MCP now
  survives restarts: the toggle is a `.edger-enabled` marker inside the
  version dir, applied at load before the `minProcesses` prewarm and winning
  over `manifest.yaml`. Before, every restart re-enabled the versions an
  operator had disabled, with their floor processes. Package installs and
  the files API cannot set or delete the marker; the state export keeps it.

## [0.3.2-rc.15]

- A worker version is bundled once and reused by every later process of
  that version (`<worker_dir>/.edger-bundle-cache/<key>/`), and the bundler
  no longer blocks an async runtime thread. Before, every new process ran
  `deno info` and `deno bundle` again (0.6-0.9 s locally and about 3.7 s in
  production for a 6 MB worker) on every prewarm, floor refill, on-demand
  spawn and recycle. An edited file invalidates the cache, the cache never
  follows a symlink, and a non-writable worker dir keeps the per-process
  bundle. `/metrics` exposes `edger_worker_bundle_total{outcome}`.

## [0.3.2-rc.14]

- A streamed body that stalls past the per-frame timeout (the worker
  `timeout`) is cancelled and drained like the total-duration cut, and the
  process is reused after a clean end. Before, every stall recycled the
  process, and an SSE stream without keepalive paid a cold start on each
  reconnect. The cut counts in `edger_stream_frame_timeout_total{outcome}`
  and emits the operational event `stream.frame_timeout` (with `timeoutMs`).
- Wasmtime and wasmtime-wasi 49.0.2 and wat 1.261.0; the WASI preview1
  sandbox, the epoch watchdog and the memory limits are unchanged.
- Rust 1.99 toolchain; `hyper` 1.12.0, `uuid` 1.27.0, `serde_json` 1.0.151.

## [0.3.2-rc.13]

- Opt-in `warmup: { path, timeout }` in the worker manifest: right after a
  `minProcesses` prewarm or floor-replenishment spawn, the pool sends one
  synthetic `GET` to the fresh process, so the first user request no longer
  pays the first-execution cost (tanstack-demo: 24-27 ms down to 9-10 ms).
  It never counts toward `maxRequests`, request metrics, the TTL or the
  circuit breaker.
- Streamed responses have a total duration limit: `streamTimeout` in the
  manifest or `EDGER_STREAM_MAX_DURATION_MS` (default 5 min; `0` disables).
  On expiry the process is cancelled and drained like an abandoned stream
  and reused after a clean end. Before, an SSE response with a heartbeat
  could hold a process forever.
- `/metrics/stats` shows each process's own `id` and its `activeRequest`.
- `POST /api/admin/workers/{name}/recycle?version=<v>` recycles the
  processes of one version without deleting it.

## [0.3.2-rc.12]

- Install (active version), promote and enable prewarm `minProcesses` in the
  background; their responses report `prewarm: "scheduled"` or
  `"not_configured"`. Before, a new version paid a cold start on its first
  request.
- A removal that empties a group with a `minProcesses` floor (a
  `maxRequests` retirement, a stream recycle, a crash) no longer drops the
  group: it stays admitted and the floor is refilled in the background.
  Before, with `minProcesses: 1` the floor was never refilled.

## [0.3.2-rc.11]

- An abandoned streamed response (client disconnect, `HEAD`, fetch abort) is
  cancelled inside the worker: the harness aborts `request.signal`, cancels the
  body and ends the response, so infinite SSE streams no longer recycle the Deno
  process. The new `cancelled` outcome shows in the lifecycle detail, the
  operational event and `edger_stream_abandoned_total`.
- Deploy precompresses immutable assets of static SPA and fullstack workers
  (`.br` at quality 11, `.gz` at level 9) and serves the variant matching
  `Accept-Encoding` with `Vary` and the same weak ETag; versions installed
  before keep live compression.
- Compression is configurable with `EDGER_COMPRESSION`,
  `EDGER_COMPRESSION_MIN_BYTES` and `EDGER_COMPRESSION_LEVEL` (chart
  `compression.*`). Only app responses answer 406 for an unsatisfiable
  `Accept-Encoding`; `HEAD` is never compressed; every 304 omits
  `Content-Length`. New `edger_http_compression_bytes_in/out_total{encoding}`.
- `minProcesses` is a maintained floor: the idle TTL keeps those instances
  and a removal below the floor starts one background replacement
  (`ttl: 0` stays ephemeral).
- `/metrics` adds `edger_worker_requests_total{...,outcome}` per worker for the
  life of the process (also `requestsTotal` in `/metrics/stats`) and the
  stream detach/abandon counters.
- `EDGER_STREAM_IDLE_MS` was removed from the docs; it was never read.

## [0.3.2-rc.10]

- The per-request dispatch log (method, path without query string, response
  content type) is on by default, and operational events print the same
  fields as `http_method`, `http_path` and `content_type`. Setting `EDGER_LOG`
  or `RUST_LOG` still replaces the default filter.

## [0.3.2-rc.9]

- When a client drops a streamed response before it ends (disconnect,
  aborted fetch, `HEAD` over a streamed page), EdgeR keeps reading and
  discarding the rest of that response and reuses the Deno process instead
  of killing it, so the next request no longer pays a cold start.
  `EDGER_STREAM_ABANDON_DRAIN_MAX_BYTES` (default 8 MiB) and
  `EDGER_STREAM_ABANDON_DRAIN_MAX_MS` (default 2000 ms; `0` in either
  disables) bound that drain. Server-sent events and other streams without
  an end are still recycled when abandoned.
- Worker dispatch logs and operational events include the request method,
  the path without query string and the response content type, and stream
  terminations record their real cause (`stream_abandoned_drained`,
  `stream_abandoned_recycled` with `bytes_limit`, `time_limit`,
  `stream_error`, `relay_timeout` or `socket_poisoned`). `drain_timeout` now
  means only a shutdown acknowledgement that did not arrive in time.

## [0.3.2-rc.8]

- A streamed response from a persistent Deno worker releases its worker slot
  as soon as the worker finishes producing it, instead of waiting for the
  client to download the whole body; the remaining bytes are delivered from
  memory. A slow client no longer makes other requests to the same worker
  time out in the queue.
- `EDGER_STREAM_DETACH_MAX_BYTES` (default 8 MiB, `0` disables) bounds that
  memory per response and `EDGER_STREAM_DETACH_TOTAL_BYTES` (default 64 MiB)
  across all responses; a response that does not fit keeps the previous
  behavior. Server-sent events and other streams without an end keep their
  slot as before.

## [0.3.2-rc.7]

- App responses are compressed with brotli or gzip when the client accepts
  it, including streamed server-rendered HTML, which is flushed chunk by
  chunk. Server-sent events, the control plane, already-compressed media,
  attachments, responses marked `Cache-Control: no-transform` and known-size
  bodies under 1 KiB are left as they are.
- A strong `ETag` set by a worker becomes weak when its response is
  compressed.
- Static SPA and fullstack files carry a weak `ETag`, and a matching
  `If-None-Match` revalidation answers 304 Not Modified without a body.
- A request whose `Accept-Encoding` accepts no supported coding receives 406
  Not Acceptable.
- wasmtime moves to 36.0.17 for RUSTSEC-2026-0321, RUSTSEC-2026-0322 and
  RUSTSEC-2026-0323.

## [0.3.2-rc.6]

- The cPanel overview now follows the selected language and shortens long
  worker identifiers in the table while preserving the full value on hover.
- The routing policy editor reflects the runtime feature flags and stays
  disabled in lab-dev while tenant and weighted routing are off.
- API key permissions can be edited without replacing the secret; the server
  checks the editor's grants and refreshes authorization after an update.
- Permission badges in the API key table are limited to two lines with a
  count and accessible list of hidden permissions.
- The API key list uses the shared DataGrid with a bounded permissions column;
  its creation action shares the page action row, and the page follows the
  selected language.
- Token sign-in shows progress only on its own button.

## [0.3.2-rc.5]

- Tenant routing adds opt-in per-app tenant allowlists resolved through
  Tenancit and weighted version rollout for requests without an explicit
  version. Both controls remain independently disabled by default.
- The cPanel console adds password and token sign-in plus administration of
  console users and their access; chart configuration can reference an
  existing Secret for the root password.

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
