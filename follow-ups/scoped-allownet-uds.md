# Follow-up: scoped `allowNet` is incompatible with the multiproc UDS transport

**Status:** deferred by decision — egress scoping isn't needed for now, so workers run with
`allowNet` unset (full net). Revisit if/when host-level egress scoping is required. The
root cause + options below are kept for that day.

## Symptom

A worker with a **scoped** `allowNet` (e.g. `["pgbouncer:6432"]`) on the persistent
multiproc backend fails to boot:

```
harness fatal: NotCapable: Requires net access to "unix:/…/w.sock", run again with --allow-net
```

Unset `allowNet` (full `--allow-net`) works. So today, DB/network workers on the
multiproc backend must use **full net** (as `tests/fixtures/param-e2e` does).

## Root cause (verified on Deno 2.9.1)

The harness connects to the orchestrator over a Unix domain socket
(`Deno.connect({ transport: "unix", path })`, `multiproc_harness.mjs`). In Deno 2.9:

- `Deno.connect(unix)` requires `--allow-net` — `--allow-read`/`--allow-write` on the
  socket path do **not** suffice (tested: fails with and without a scoped net list).
- `--allow-net` **cannot scope a unix path**: `--allow-net=host:port,/path.sock` →
  `invalid host '/path.sock': invalid char found in FQDN`.

So the only grant that covers the UDS connect is **full `--allow-net`**. A scoped
`--allow-net=host:port` (from the manifest hosts) excludes the UDS → boot fails.
`deno_sandbox_policy.rs::deno_network_permission_args` produces the scoped list.

## Options (each is a real trade-off)

1. **fd-passing** — the orchestrator passes the already-connected socket fd to the
   Deno process; the harness wraps the inherited fd instead of `Deno.connect`. No net
   permission needed for the internal channel. Correct long-term, but Deno has no
   ergonomic API to wrap an arbitrary inherited fd as a `Conn` — non-trivial.
2. **TCP loopback for the internal channel** — bind `127.0.0.1:0`, harness connects to
   `127.0.0.1:<port>`, and the spawn adds `127.0.0.1:<port>` to the scoped `--allow-net`.
   Small code change, and scoping then works — BUT a loopback port is less isolated than
   a `0700` unix socket (any local process can connect during the worker's life). A
   security downgrade for a multi-tenant data plane.
3. **edger-level egress policy** — keep the UDS + full Deno `--allow-net`, and enforce the
   manifest's host allowlist at an edger layer (egress proxy / filter) instead of relying
   on Deno's `--allow-net` scoping. Most work; keeps both isolation and scoping.

## Recommendation

Do not rush a transport change. Keep the full-net workaround for now; pick (1) or (3)
when hardening egress scoping. Track here.
