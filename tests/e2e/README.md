# edger Track B — end-to-end system test

Reproducible e2e for the runtime capabilities added for the `parameters-v2` app:

- **B1** — server workers receive all declared env (`DATABASE_URL`); the worker
  queries Postgres **via PgBouncer** (transaction mode).
- **B2** — `beforeunload` + `EdgeRuntime.waitUntil` drain the pool on graceful
  shutdown (SIGTERM) and on TTL/idle recycle, before the process is killed.
- **B3** — the **release phase** runs migrations once per version before serving.

`Deno.openKv()` is enabled for workers (`--unstable-kv`); the app picks the
backend itself. Note: a local SQLite KV is per-pod and ephemeral in k8s — durable
shared storage needs a remote backend (Turso/libSQL or KV Connect).

## Run

```bash
tests/e2e/run.sh
```

It builds `./target/debug/edger`, brings up Postgres + PgBouncer on a fresh
volume (`docker-compose.yml`), boots edger against `tests/fixtures/param-e2e`, and
asserts each capability, then tears everything down. Exits non-zero on failure,
`SKIP` (0) if docker/deno are unavailable.

Requires: `docker`, `deno`, `cargo`.

## What proves what

| Assertion | Proves |
|---|---|
| `/param-e2e/health` → `{"ok":1}` | worker got `DATABASE_URL`, `select 1` via PgBouncer (B1) |
| `e2e_release_marker` + `_migrations` ≥ 2 + `.edger-release` | release ran migrations once (B3) |
| `/param-e2e/?tenant=…` → `count:3` | parameterized query (`$1`) via PgBouncer (B1) |
| `e2e_shutdown_log` row `reason=terminate` | beforeunload drain fired on graceful SIGTERM (B2) |

## Complementary in-repo unit/integration tests

These run under plain `cargo test` (the ones needing a Deno process are gated by
`--features multiproc`):

- B1 — `crates/edger-orchestrator/tests/kind_dispatch_integration.rs::deno_backend_injects_all_manifest_env`
- B2 — `crates/edger-isolation/tests/uds_roundtrip.rs::graceful_shutdown_dispatches_beforeunload_and_drains_wait_until`
- B3 — `crates/edger-orchestrator/src/deploy.rs::release_tests`

## Known follow-ups (surfaced by this e2e)

- Scoped `allowNet` breaks the multiproc backend (harness needs net to its unix
  socket; Deno can't scope a unix path in `--allow-net`). This worker uses full net.
