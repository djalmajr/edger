# Changelog

All notable changes to EdgeR will be documented here.

## [Unreleased]

Toward 0.3.2. Release candidates: `v0.3.2-rc.29` (chart `startupProbe`
and pinned Deno image);
`v0.3.2-rc.28` (`ctutils` 0.4.5
and `tokio` 1.54.1);
`v0.3.2-rc.27` (`rustls-webpki`
0.103.16 and `webpki-roots` 1.0.10);
`v0.3.2-rc.26` (files upload all or
nothing and never through a symbolic link, `wasmtime` 49.0.4,
OpenTelemetry 0.33);
`v0.3.2-rc.25` (EdgeR's own path
check on package extraction, malformed zip reads as 400);
`v0.3.2-rc.24` (egress guard for
user-chosen hosts and app secrets vault for BYOK, checked duration
parsing and `cron[]` limits); `v0.3.2-rc.23` (`zip` 9.0.4);
`v0.3.2-rc.22` (cron jobs listed with
their next run, expired API keys no longer authenticate from the cache);
`v0.3.2-rc.21` (the worker's
`request.url` carries the real origin on its own host, resolution errors
no longer reveal a hidden worker); `v0.3.2-rc.20` (console hash slot held
until the calculation ends, tenant gate errors no longer reveal a hidden
worker, fallback bundle never cached nor served broken); `v0.3.2-rc.19` (root-only worker
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

- Egress guard for user-chosen hosts (EDG-38): an app that must call a host
  the user picked (Pinar's BYOK) cannot put that host in `allowNet` without
  also letting the worker leave through it directly, so the EdgeR serves the
  call itself. Manifest: `egress: {enabled, ports (default [443]; HTTPS
  only), connectTimeout (5s), headersTimeout (30s), idleTimeout (30s),
  totalTimeout (5m), maxResponseBytes (100mb), maxConcurrent (8)}`; with
  `enabled: true` the manifest must declare `allowNet` (it may be `[]`).
  Each Deno process of such a version gets its own Unix socket
  `guard.sock` in its workdir, the `--allow-net` of that process gains
  `unix:<guard.sock>` and `edger-egress.invalid:80` (the manifest `allowNet`
  is untouched), and the env gains `EDGER_EGRESS_SOCKET` (the socket path)
  and `EDGER_EGRESS_HOST` (`edger-egress.invalid`, a host that never
  resolves). The variables exist only while the socket is live: with no
  guard service, a failed bind or `EDGER_JS_RUNTIME=bridge` neither the
  socket nor the variables appear, one `warn` per worker and version is
  logged and the worker starts normally (a manifest `env` or a secret named
  like the two variables is dropped, in the persistent process runtime and in
  the bridge alike, so the presence of the socket cannot be faked). The task
  serving a socket ends with its process (recycle, TTL, LRU,
  shutdown) and the socket disappears with the workdir; a staged version gets
  the guard like any other (the spawn does not look at the default pointer).
  Contract: the app calls `fetch("http://" + EDGER_EGRESS_HOST + "/")` through
  `Deno.createHttpClient({ proxy: { transport: "unix", path:
  EDGER_EGRESS_SOCKET } })` with the destination in the `x-edger-egress-url`
  header; method, headers (the user's `Authorization` included) and body
  (up to 8 MiB) go through, the response (status, headers, streaming body,
  SSE included) comes back as the destination sent it. The guard resolves the
  name, classifies EVERY resolved IP (private, loopback, link-local, CGNAT,
  cloud metadata, cluster, node, documentation, reserved; IPv4 and IPv6,
  mapped and odd notations), dials the classified IP only (no second
  resolution), follows up to 5 redirects revalidating each hop and dropping
  `Authorization`/`Cookie` on a cross-origin hop. A refusal carries
  `x-edger-egress-error: <code>` and a JSON `{"error","message"}` body:
  `EGRESS_BAD_TARGET` 400, `EGRESS_PORT_BLOCKED` 403, `EGRESS_HOST_BLOCKED`
  403, `EGRESS_DNS_ERROR` 502, `EGRESS_UPSTREAM_ERROR` 502,
  `EGRESS_TOO_MANY_REDIRECTS` 502, `EGRESS_RESPONSE_TOO_LARGE` 502,
  `EGRESS_TIMEOUT` 504, `EGRESS_LIMIT` 429 (`maxConcurrent` is per worker
  version, shared by its processes, no queue), `EGRESS_REQUEST_TOO_LARGE`
  413. A cut after the response head (body over `maxResponseBytes`,
  `idleTimeout`, `totalTimeout`, the destination dropping) ends the stream
  with an error, never a silent truncation. Each call records one
  `worker.egress` operational event (worker, version, process id, outcome,
  status, duration, code; destination host and port, connected IP, bytes and
  redirect count ride the `message`) and never a header (name or value), a
  body or a query string; the per-worker counters
  `edger_egress_requests_total{worker,outcome}` and
  `edger_egress_response_bytes_total{worker}` appear at `/metrics` once a
  guard is configured (no guard, no line). Operator variables:
  `EDGER_EGRESS_BLOCK_CIDRS`
  (comma list of extra CIDRs to refuse; empty keeps the k3s pod and service
  ranges `10.42.0.0/16`, `10.43.0.0/16`), `EDGER_NODE_IP` (the node IP, also
  refused) and, for local development ONLY, `EDGER_EGRESS_ALLOW_CIDRS` (CIDRs
  the classification would refuse, such as `127.0.0.0/8`; empty by default,
  a `warn` at the first use, never set it in production). The Helm chart sets
  `EDGER_NODE_IP` from the downward API (`status.hostIP`) and has an optional
  `egress.blockCidrs` list that becomes `EDGER_EGRESS_BLOCK_CIDRS`; the dev
  variable is not exposed. The bridge fallback (`EDGER_JS_RUNTIME=bridge`)
  and wasm workers have no guard. Details and the reasoning:
  `planning/edger/docs/egress-byok.md`.

- App secrets vault (EDG-38): apps store secrets (e.g. Pinar's
  `BYOK_MASTER_KEY`) encrypted at rest in a dedicated SQLite store
  (`app_secrets(worker, name, nonce, ciphertext, created_at, updated_at)`),
  ChaCha20-Poly1305 with a random nonce per write and AAD =
  `worker || 0x00 || name` (a ciphertext copied to another worker/name pair
  does not decrypt). Admin routes: `PUT /api/admin/workers/{name}/secrets/{secret}`
  (`{"value"}`; non-empty, at most 16 KiB, name follows the manifest rule,
  the worker does NOT need to be installed), `GET /api/admin/workers/{name}/secrets`
  (names and dates only — there is no route that returns the value) and
  `DELETE /api/admin/workers/{name}/secrets/{secret}`. New catalog
  permissions `secrets:read` (list) and `secrets:write` (write and delete),
  under the existing anti-escalation and the per-worker scope of the API key;
  root covers every worker. Workers declare names in `secrets:` and the
  process backend injects each resolved value into the env of the spawned
  worker process only (never in `WorkerConfig.env` or `publicEnv`); a
  declared-but-absent secret logs one warn per worker and name and the
  worker starts normally; a record that fails to decrypt (wrong or rotated
  master key) fails closed — the secret is not injected and the error log
  never carries the value. Master key: 32 bytes from `EDGER_SECRETS_KEY_FILE`
  (base64 or raw), store in `EDGER_APP_SECRETS_DB` (default
  `<first worker dir>/.edger/app-secrets.db`). Without the key file the
  instance still boots: the vault is unavailable, the routes answer 503
  `SECRETS_UNAVAILABLE`, nothing is injected and a warn is logged at boot.
  A repeated `PUT` takes effect at the next spawn; apply immediately with
  `POST /api/admin/workers/{name}/recycle?version=<v>` (the write never
  recycles on its own). Operational events `secret.written` and
  `secret.deleted` carry worker, secret name and acting API key id — never
  the value. The state export ships a consistent encrypted copy as
  `app-secrets.db` (the master key itself is never exported). The Helm
  chart gains an opt-in `appSecrets` block (`enabled`, `dbPath`,
  `keySecret: {name, key}`) that mounts an existing Secret with the master
  key read-only and sets `EDGER_SECRETS_KEY_FILE`; the bridge fallback
  (`EDGER_JS_RUNTIME=bridge`) does not inject and logs a warn per declared
  secret. Security-model note: anyone who can install a worker version can
  read the secrets injected into that worker through its process env — the
  value is protected by the deploy permission, not by `secrets:read`.

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
- `ConsoleAuthService` exposes `max_observed_hash_concurrency()`: the peak
  number of concurrent Argon2 password calculations observed since the
  service started (operational diagnostic of the shared hash-slot limit —
  the slot is reserved before the calculation and stays held until it
  finishes, so the peak never exceeds the configured `with_hash_concurrency`
  ceiling).

- Cron scheduler visibility (EDG-34):
  - `GET /api/admin/cron/jobs` (permission `observability:read`) lists the
    tracked jobs ordered by worker, schedule, path and method, each with
    `version` (the version the unversioned route resolves now, `null` when
    it cannot), `state` (`active` or `draining`) and `nextRunAt` (RFC 3339
    UTC). The key's worker scope filters the list with the same predicate as
    `GET /api/admin/workers`, applied to the job's worker name, so a
    draining job of a deleted worker stays visible until it is reaped.
    `enabled` is `true` only while the scheduler runs with the root Bearer
    configured.
  - `nextRunAt` is the deadline the job task actually sleeps until, kept as
    a monotonic instant and converted to UTC when read, so a wall-clock
    step moves the published time but not the tick. It is `null` while the
    job drains, before its first sleep, and when the deadline cannot be
    represented or falls after year 9999. Time arithmetic is checked, so a
    huge valid `@every` interval no longer panics the shared supervisor.
  - `/metrics` gains the count-only gauges
    `edger_cron_jobs{state="active"|"draining"}`.
  - Job-set changes log one `info` line each: `cron job started`,
    `cron job stopped` (drain start) and `cron job drained` (reaped), with
    the resolved version. Every cron log line writes `worker`, `schedule`,
    `path`, `method` and errors Debug-escaped, and the manifest validation
    refuses control characters in `cron[]` `path`, `schedule` and `method`,
    so manifest text can no longer forge log lines.
  - The listing and the change lines resolve each worker's version once per
    request or supervisor pass (`ManifestIndex::resolve_default_version`,
    no `WorkerRef` clone), so many `cron[]` entries cost linear time
    instead of quadratic.

### Changed

- Production platform (EDG-25): the shared `sqld` memory limit is 1 Gi
  (request 128 Mi) and the backup jobs get 1 CPU and an 1800 s
  deadline, sized from the 2026-10-15 rehearsal; the runbook records
  the rehearsal times, the batched restore, warming a staged version by
  pin before promote, and secrets written without a trailing newline.

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

- The files upload is all or nothing (EDG-39): `POST
  /api/admin/workers/{name}/files` used to extract the zip straight into the
  version dir, so an entry refused by path (`DEPLOY_PATH_DENIED`), a read
  error mid-archive (`DEPLOY_INVALID_PACKAGE`), a file/directory type
  conflict or an I/O error left the earlier entries written while the CAS
  revision stayed the same. The upload now extracts and validates
  everything in a `.edger-upload-*` staging dir next to the version, then
  publishes the entries in archive order, each file by one atomic rename
  over its target; an overwritten file is first kept as a hard link (a copy
  where linking fails and the file has no other link) in a backup dir, so
  the old file stays visible until the new one replaces it. Any failure
  restores the published replacements, removes what was created (a new
  `?path=` subfolder included, created level by level) and returns the
  original error. Error codes and messages, permissions (a read-only file
  is still refused), file modes and the success body are unchanged; a
  target that is not a regular file (a FIFO, socket or device) is refused
  without being opened.
  `.edger-upload-*` dirs are never indexed as a version (not even one left
  by a process that died mid-upload) and are skipped by the state export;
  the install's `.edger-install-*` staging dirs are no longer indexed
  either.

- Size values no longer overflow (EDG-38): a size whose byte value does not
  fit `u64` (for example `17179869184gb`, 2^34 GiB) panicked the manifest
  parsing in debug builds and wrapped to a small value in release builds.
  `parse_size_to_bytes` now uses checked multiplication and the value is
  invalid, exactly like any other unparseable size: `maxBodySize` falls
  back to its default (4 MiB) and the new `egress.maxResponseBytes` is
  rejected at manifest validation (`must be a size such as 100mb`). An
  `egress.maxResponseBytes` of `0` is also rejected now (`must be greater
  than 0`).

- Duration values no longer overflow (EDG-36): a duration whose millisecond
  value does not fit `u64` (for example `18446744073709552s` / a bare
  `18446744073709552` seconds, or a `cron` `@every` interval near the same
  magnitude) panicked the cron supervisor in debug builds and wrapped to a
  small value (384 ms) in release builds. The parser now uses checked
  multiplication and the value is invalid, exactly like any other
  unparseable duration (the field's default applies, or the `@every` job is
  skipped with a `warn`). It covers `ttl`, `streamTimeout` and `@every`
  (every duration field going through the checked parser).

- Manifest `cron[]` limits (EDG-36): at most 32 `cron[]` entries per
  manifest (`cron must have at most 32 entries`) and an `@every` interval of
  at least 1 s (`cron @every interval must be at least 1s`), rejected at
  manifest validation — so the install, the boot index load and the rescan
  refuse a manifest that crosses either limit; the scheduler also treats a
  sub-1 s `@every` as an invalid schedule (defense in depth). An `@every`
  the duration parser does not accept is still not rejected at validation;
  the scheduler skips it with a `warn`, as before. Before upgrading, check
  that no version the scanner admits crosses either limit: an invalid
  manifest in an admitted version (an enabled version, a version disabled
  only by the persisted `.edger-enabled` marker, or an admitted rollback
  version) aborts the whole `edger` process at boot (the index is never
  built), and a rescan answers 400 without changing the index. A version
  whose own manifest says `enabled: false` (without a marker saying
  `enabled=true`) is skipped before validation and cannot block the boot.

- Expired API keys no longer authenticate from the cache (EDG-35): a key
  cached while still valid kept authenticating for up to 60 s after its
  `expires_at`. `try_authenticate` now re-checks the cached deadline with
  the store's clock and comparison and drops the expired entry, so the
  next request is rejected. Revocation stays immediate.

- Production control-plane route (EDG-25): the IngressRoute rule is a
  single line, because the cluster's Traefik rejects a rule with line
  breaks (`invalid rule`), which left `edger.djalmajr.dev` without a
  route for 44 s during the 2026-10-15 cutover.

- Console hash slots stay held until the end of the calculation (EDG-28):
  the shared Argon2 limit's permit is moved inside the `spawn_blocking`
  closure and released only when the calculation completes, alongside the
  in-flight counter guard, so a client that disconnects mid-login no longer
  frees the slot while the calculation keeps running — abandoned
  connections can no longer push concurrent password calculations past the
  configured limit (the acquisition timeout, the `CONSOLE_BUSY` 503 and the
  login statuses are unchanged).

- The minimal relative-module fallback bundle is never published to the
  per-version bundle cache (EDG-30): when `deno bundle` fails on a registry
  error, the fallback output (a single file) is validated with the same
  mechanism the bundler already uses before `deno bundle` — a `deno info
  --json` dependency graph of the output file, run from the directory it
  will be executed in, with the same Deno executable that produced the
  original error. The real Deno parser catches a relative import left in
  the output in any form (minified `import{a}from"./x"`, re-export, comment
  `import(/* c */"./x")`, template `` import(`./x`) ``, escape
  `import("\x2e/x")` or dynamic `import("./x")`) without flagging method
  calls (`o.import("./a")`) or template text that merely looks like an
  import. A correct fallback embeds every relative module, so a `file:`
  code dependency left in a module REACHABLE from the graph roots BY CODE
  EDGES that points at a `file:` module different from the output file
  itself (the graph root) — whether or not it exists or resolves — or a
  broken output file fails the fallback with `DENO_BUNDLE_FAILED` (the
  original registry cause plus the first surviving `file:` specifier), so a
  broken bundle can no longer be cached and fail every request of the
  version; the rule no longer depends on the directory the graph is
  inspected in (the staging dir is one level deeper than the per-process
  dir the bundle is finally materialized in); a fully-resolved fallback
  serves only the current spawn (per-process, counted `uncached`) and the
  next spawn retries the real `deno bundle`. Type-only edges never count —
  a subgraph reachable only by type edges never executes, so its code edges
  are exempt from the rule — and remote / `npm:` / `jsr:` targets keep the
  previous behavior.
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
- The per-worker Deno cache no longer accumulates unboundedly in
  `${TMPDIR}/edger-deno-cache` (EDG-32): the per-worker cache root can now be
  set explicitly on the process isolate (`DenoProcessIsolate::with_per_worker_deno_cache_root`),
  taking precedence over `EDGER_DENO_CACHE_ROOT` (without it the previous
  rule — env var, then the shared temp dir — is unchanged), and every test
  that spawns real Deno processes in per-worker mode keeps its cache inside
  its own tempdir instead of the shared one.

- The worker's `request.url` carries the real origin on a vhost dispatch
  (EDG-33): when a request reaches the worker through its own host (the
  `Host`/authority matched one of the version's `hosts`), the orchestrator
  sends the ABSOLUTE URI `<scheme>://<routed host><path and query>` on the
  isolate wire — the routed host is exactly the matched authority (with the
  port when the client sent one) and the scheme comes from `X-Forwarded-Proto`
  (`https` when the first value of the list is `https`, case-insensitive;
  `http` in every other case, including absent or invalid). Only the
  process-backend kinds receive the absolute form, decided by the FINAL
  worker's kind: fetch, routes and fullstack — the persistent process
  harness and the legacy CLI bridge pass a scheme-bearing URI through to
  the worker's `new Request`, and the fullstack glue inserts the base AFTER
  the authority (`https://app.example` + `/app` + `/x?q=1`, for the
  uri-rewriting adapters; hono/lume read the worker-relative `x-base` as
  today) while asset and
  base matching still see only the path — so a worker that compares
  `Origin` with the origin of `request.url` (a common CSRF protection) no
  longer sees `http://edger.local` and refuses every browser POST, and
  absolute links are emitted with the real origin. Static SPA and WASM keep
  the worker-relative URI (their resolvers read the path relative to the
  worker dir), and every other path keeps the relative URI and the
  `http://edger.local` placeholder: the name route, the `@version` pin,
  the homepage fallback, the plugin base on an owner-less host, the
  authenticated control-plane invoke, cron, health check and warmup.
- EDG-42: a slow boot is no longer killed by the liveness probe. The chart
  adds a `startupProbe` on `/livez` (`probes.startup.enabled`, default
  `true`; `periodSeconds` 5, `timeoutSeconds` 1, `failureThreshold` 60, so
  5 minutes), and liveness and readiness only start after it succeeds. The
  bundle cache key includes `deno --version`, so an image with a new Deno
  rebuilds the bundle of every admitted version before the server listens
  (~53 s for 12 versions in production on 2026-10-22, when the liveness
  probe killed the first container and the apps answered 503 for about
  70 s). The image now pins Deno (`denoland/deno:debian-2.9.8` in
  `Dockerfile` and `Dockerfile.cross`), so a Deno upgrade is an explicit
  pull request.

### Security

- `wasmtime`/`wasmtime-wasi` 49.0.3 -> 49.0.4 (floor raised to 49.0.4),
  with the matching `cranelift` 0.136.4, `pulley` and `wiggle` patch
  releases: RUSTSEC-2026-0378 (panic lowering a host-created component
  model stream into a guest that expects a different payload type) and
  RUSTSEC-2026-0379 (the Cranelift aarch64 backend can jump to the wrong
  target on a large Wasm `br_table`). Both apply to the wasm worker
  backend.
- Package extraction could write outside its staging directory in
  0.3.2-rc.22 and earlier: `zip` 8.6.0 turned an entry name with a Windows
  verbatim prefix into a relative path with `..`, which escaped the
  destination on Unix. A key with `workers:install` or `files:write`, even
  one scoped to a single worker, could write anywhere the EdgeR process can
  write: other workers, the API key database and the core worker overlays.
  Fixed in 0.3.2-rc.23 by `zip` 9.0.4, which refuses those names as
  `DEPLOY_PATH_DENIED`. Upgrade, and review the keys that hold those
  permissions.

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

- Hidden workers no longer leak existence through tenant-gate errors (EDG-29):
  the EDG-27 route refusal now runs before the tenant gate, so a worker whose
  final version declares `hosts` (without `nameRoute: true`) answers
  `404 NOT_FOUND` in the same shape as an unknown worker on the owner-less
  name route (with or without the `@version` pin), the homepage fallback and
  a weighted selection of a protected version — even when the tenant gate
  would deny or the identity is unavailable — and the identify call is never
  made. The tenant gate still fails closed on the worker's own hosts, and
  `nameRoute: true` workers still pass through it.

- Resolution errors no longer reveal a hidden worker on owner-less routes
  (EDG-31): on a public owner-less origin (name route, `@version` pin,
  homepage fallback and plugin base), when NO version reachable by a public
  name/pin route is admitted by the EDG-27 route guard there — every
  reachable version (enabled and public, including enabled staged, which a
  pin such as `@latest` reaches) declares `hosts` without `nameRoute: true`,
  and disabled or internal versions never count — the resolution failures
  that happened before the guard (a pin to a version that does not exist, a
  malformed semver pin requirement, and an unavailable weighted selection)
  answer `404 NOT_FOUND` in the same status, body, `content-type` and
  `content-length` as an unknown worker of the same name size, with or
  without tenant routing, without calling identify and without falling back
  to the default version. The route refusal and the normalized failure on a
  homepage answer exactly what the absence of that route answers
  (`no homepage configured`), never the app's real name. On a plugin base
  the refusal is not imitated: the SAME original path is re-resolved through
  the router excluding that base and dispatched through the normal flow
  (weighted selection, route guard and tenant gate of the new route), so the
  answer is exactly what the router answers without the base — the next
  matching base, the nominal route, the homepage or its absence, or the
  router's own errors (the `HOMEPAGE` signal and `PARSE_ERROR` included) —
  and the hidden app never receives the dispatch. Internal requests (root),
  the worker's own hosts and `nameRoute: true` versions keep being served by
  the alias. The detailed errors (`version X not available`,
  `invalid semver requirement`, `503 application not available`) remain for
  apps with at least one admitted reachable version (no `hosts`, or
  `nameRoute: true`), on the worker's own hosts and for internal requests.

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
- EDG-37: package extraction (install and files upload) no longer rests on
  the `zip` library alone to keep entries inside the destination. After
  `enclosed_name`, EdgeR accepts a path only when every component is a
  plain name (no `..`, root, drive or verbatim prefix, `.`, `\` or NUL, and
  not empty) and re-checks that the joined target stays under the
  destination, all refused as `DEPLOY_PATH_DENIED`. A directory entry that
  points at the package root (`./`) is still ignored. A zip entry that cannot
  be read (declared size smaller than the content, wrong CRC, invalid
  deflate) now answers 400 `DEPLOY_INVALID_PACKAGE` instead of 500
  `DEPLOY_IO`.
- EDG-40: the files upload (`POST /api/admin/workers/{name}/files`) never
  follows a symbolic link below the version directory, in an intermediate
  or in the final component, even one planted from outside EdgeR and even
  one pointing back inside the version. A `?path=` segment that is a link is
  403 `FORBIDDEN` (`path crosses a symbolic link`), including a link in a
  cycle or to a target without search permission; a zip entry that meets a
  link is 400 `DEPLOY_PATH_DENIED` (`zip entry crosses a symbolic link:
  <entry>`). A refusal, including one caused by a link, undoes the whole
  upload; only a concurrent change that blocks the rollback itself (a
  version directory turned into a link mid-upload) leaves that step undone,
  logs a `warn` and keeps the backup dir for manual recovery. A link that was
  the final target is no longer written through. On the version side every
  operation (path walk, directory creation, write probe, backup, publication
  and rollback) is relative to directory descriptors opened one component at
  a time with `O_NOFOLLOW`, so a link swapped in between a check and the
  write fails the open instead of being followed. The guarantee is that the
  write never follows a link and acts on the directories opened at the
  start; since a descriptor pins the inode, not its place in the tree, an
  outside process that moves one of those directories out of the version
  during the upload is not stopped by it, and guarding against that is up
  to whoever has access to the node's filesystem.

### Dependencies

- `wasmtime` and `wasmtime-wasi` 49.0.4 -> 49.0.5 for RUSTSEC-2026-0422,
  RUSTSEC-2026-0423 and RUSTSEC-2026-0424 (the `cranelift`, `pulley` and
  `wiggle` crates move with them).
- `zip` 8.6.0 -> 9.0.4 for RUSTSEC-2026-0367, RUSTSEC-2026-0368 and
  RUSTSEC-2026-0369. Those advisories cover `ZipArchive::extract()` and
  `ZipStreamReader::extract()`, which EdgeR does not call: the install
  and the files upload extract entry by entry through `enclosed_name`,
  write plain files and apply no Unix mode and no symlink from the zip.
  The new major also validates more, so some malformed packages that
  were accepted before are now refused, among them:
  - an entry that holds more bytes than its declared uncompressed size
    (zip 9.0.1). A manifest entry that under-declares its size and goes
    past the manifest cap is now `400 DEPLOY_INVALID_PACKAGE`
    (`failed to read manifest entry: ...`) instead of
    `413 PAYLOAD_TOO_LARGE`; during extraction the same read fails as
    `DEPLOY_IO`;
  - an entry name with a separator inside a Windows verbatim path
    component (zip 9.0.4), now refused as `DEPLOY_PATH_DENIED`.

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
- `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp` and
  `opentelemetry-appender-tracing` 0.32 -> 0.33.1 and
  `tracing-opentelemetry` 0.33 -> 0.34.0, together, behind the optional
  `otel` feature (compiled into the release images; the export only runs
  when `EDGER_OTEL_ENABLED` and an OTLP endpoint are set, which the
  production values do not do); same signals, features and
  `OTEL_*` variables. Upstream change kept: the OTLP exporters now retry a
  failed export up to 3 times with backoff (off the request path, in the
  batch processor).

## [0.3.2-rc.29]

- The chart adds a `startupProbe` on `/livez` (`probes.startup.*`, default
  60 checks every 5 s), so a boot that rebuilds every bundle is no longer
  killed by the liveness probe, and the image pins Deno
  (`denoland/deno:debian-2.9.8`) (EDG-42, #162).

## [0.3.2-rc.28]

- `ctutils` 0.4.5 (RUSTSEC-2026-0385: non-constant-time mask generation and
  `u128` comparisons), reached through `chacha20poly1305`, the app secrets
  vault cipher.
- `tokio` 1.54.1 (RUSTSEC-2026-0386: `AsyncFd::new` and related constructors
  violate I/O safety).
  Both are lockfile-only bumps (#160).

## [0.3.2-rc.27]

- `rustls-webpki` 0.103.16 (RUSTSEC-2026-0382) and `webpki-roots` 1.0.10
  (RUSTSEC-2026-0381) for the outbound TLS clients (`reqwest`/`hyper-rustls`
  in `edger-orchestrator` and `edger-mcp`, and the egress guard's `rustls`
  client).
  Lockfile-only patch bumps.

## [0.3.2-rc.26]

- The files upload (`POST /api/admin/workers/{name}/files`) is all or
  nothing (EDG-39): the zip is validated in a `.edger-upload-*` staging dir
  next to the version and each file is published by one atomic rename, with
  the overwritten files kept in a backup dir; any refusal or failure undoes
  what was applied, with the same error codes and messages as before. The
  scanner no longer indexes `.edger-upload-*` or the install's
  `.edger-install-*` staging dirs.
- The files upload never follows a symbolic link below the version
  directory (EDG-40): every write is relative to directory descriptors
  opened with `O_NOFOLLOW`; a link in `?path=` answers `403 FORBIDDEN` and a
  zip entry that meets a link answers `400 DEPLOY_PATH_DENIED`.
- `wasmtime`/`wasmtime-wasi` 49.0.4 (RUSTSEC-2026-0378, RUSTSEC-2026-0379),
  with the matching `cranelift` 0.136.4 patch releases.
- `opentelemetry` family 0.33.1 and `tracing-opentelemetry` 0.34.0 behind the
  optional `otel` feature (export still off unless `EDGER_OTEL_ENABLED` and an
  OTLP endpoint are set).

## [0.3.2-rc.25]

- Package extraction (worker install and files upload) checks every entry
  path itself after zip's `enclosed_name()`: only plain components, no
  backslash or NUL, never empty, and the target must stay inside the
  destination, refused as `DEPLOY_PATH_DENIED` (EDG-37). A directory entry
  that points at the package root (`./`) is still ignored. A zip entry that
  cannot be read (bad CRC, more bytes than declared, invalid deflate) now
  answers `400 DEPLOY_INVALID_PACKAGE` instead of `500 DEPLOY_IO`.

## [0.3.2-rc.24]

- Egress guard for user-chosen hosts (EDG-38): a worker version with
  `egress.enabled` reaches HTTPS hosts picked by its users only through the
  EdgeR, which resolves the name, refuses internal, metadata, cluster and
  node addresses, dials the checked IP, revalidates redirects and enforces
  timeouts, response size and concurrency per worker version. See the
  `[Unreleased]` entry for the contract and the operator variables.
- App secrets vault (EDG-38): secrets encrypted at rest
  (ChaCha20-Poly1305), written through `PUT /api/admin/workers/{name}/secrets/{secret}`
  (`secrets:write`; the value is never returned) and injected only into the
  env of the worker's process. Needs the master key Secret and the chart's
  `appSecrets` block; without the key the instance boots with the vault
  unavailable.
- Duration values no longer overflow, and the manifest validation caps
  `cron[]` at 32 entries with an `@every` of at least 1 s (EDG-36). Before
  upgrading, check that no version the scanner admits crosses those limits.
- Production manifests: the `pinar-stg` database objects are gone, and the
  production values enable the app secrets vault.
- Dependencies: serde_json 1.0.154, http 1.5.0, serde 1.0.229, base64 0.23.1
  and oven/bun 1.4.3.

## [0.3.2-rc.23]

- `zip` 8.6.0 -> 9.0.4 (RUSTSEC-2026-0367 to 0369). The new version
  refuses more malformed packages: an entry that holds more bytes than
  its declared size (a manifest entry past the manifest cap is now
  `400 DEPLOY_INVALID_PACKAGE` instead of `413 PAYLOAD_TOO_LARGE`) and an
  entry name with a separator inside a Windows verbatim path component
  (`DEPLOY_PATH_DENIED`).
- Security: package extraction (worker install and files upload) no
  longer writes outside its staging directory for an entry name with a
  Windows verbatim prefix, which 0.3.2-rc.22 and earlier accepted (see
  Security under `[Unreleased]`).

## [0.3.2-rc.22]

- `GET /api/admin/cron/jobs` (permission `observability:read`, filtered by
  the key's worker scope) lists the tracked cron jobs with the version the
  unversioned route resolves now, `state` (`active` or `draining`) and
  `nextRunAt`, the deadline the job task really sleeps until; `/metrics`
  gains `edger_cron_jobs{state="active"|"draining"}`, and every job-set
  change logs one `info` line (`cron job started`, `stopped`, `drained`).
  Cron log lines escape manifest text, and the manifest validation refuses
  control characters in `cron[]` `path`, `schedule` and `method` (EDG-34).
- An API key that expires while cached is rejected on the next request,
  instead of authenticating for up to 60 s after its `expires_at` (EDG-35).

## [0.3.2-rc.21]

- When a request reaches a worker through its own host, the fetch, routes
  and fullstack workers receive `request.url` with the real origin
  (`<scheme>://<routed host><path and query>`, the scheme from the first
  `X-Forwarded-Proto` value, `https` or `http`) instead of
  `http://edger.local`, so an app that compares `Origin` with the origin of
  `request.url` accepts the browser POST; static SPA and WASM workers and
  every other route keep the worker-relative URI (EDG-33).
- On a public owner-less origin, a worker hidden from those routes no longer
  leaks through resolution errors: a missing or malformed `@version` pin and
  an unavailable weighted selection answer like an unknown worker, a hidden
  homepage answers like no homepage, and a plugin base that does not admit
  the request is resolved again without that base, so the answer is exactly
  what the router answers without it (EDG-31).
- Tests that spawn real Deno processes keep their own per-worker Deno cache
  instead of growing the shared temporary directory (EDG-32).

## [0.3.2-rc.20]

- Console hash slots stay held until the Argon2 calculation ends, so a
  client that disconnects mid-login no longer frees the slot while the
  calculation keeps running and abandoned logins can no longer exceed the
  configured limit; the login serialization test checks the observed peak
  instead of a millisecond margin (EDG-28).
- With tenant routing on, a worker hidden from owner-less public routes
  answers like an unknown worker instead of with the tenant gate error, and
  the identify service is not called for it (EDG-29).
- When `deno bundle` cannot reach the npm registry, the minimal fallback
  bundle is never published to the per-version bundle cache, and a fallback
  that still has a local `file:` code dependency fails with
  `DENO_BUNDLE_FAILED` (validated with `deno info`) instead of a module that
  breaks at runtime (EDG-30).
- Production platform (EDG-25): shared `sqld` limit 1 Gi, backup jobs with
  1 CPU and an 1800 s deadline, a single-line control-plane route rule, the
  Pinar backup sentinel, and the rehearsal and cutover results in the
  runbook.

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
