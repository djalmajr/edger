# Egress guard (BYOK): contract and design (EDG-38)

How a worker reaches a host the USER chose (Pinar's bring-your-own-key calls
to `ai-gateway.vercel.sh`-like providers) without being able to reach anything
else. Implemented by EDG-38 S1 (IP classification, `edger-core`), S2a (the
service, `edger-orchestrator/src/egress_guard.rs`), S2b (the spawn wiring,
`edger-isolation/src/multiproc.rs`) and S3 (app secrets vault, separate doc
section in `CHANGELOG.md`).

## Why a service of the EdgeR and not a proxy in `allowNet`

In Deno 2.9.7 (the production image `0.3.2-rc.23`) EVERY proxy of `fetch`
(HTTP `CONNECT`, SOCKS5, TCP and Unix socket) checks the net permission
against the DESTINATION (the host of the URL), not against the proxy. This was
measured, not assumed (ai-memory gotcha `gotchas/deno-proxy-permissao-no-destino`,
cited here as text).

Consequences:

- A `CONNECT` proxy with end-to-end TLS would need the user's host in the
  worker's `--allow-net`, and then the worker could connect to it directly,
  bypassing the guard. The guard would be advisory.
- With the destination NOT allowed, any real host fails with `NotCapable`
  even through a proxy; with a host that never resolves (`edger-egress.invalid`)
  the direct path fails in DNS.

So the guard is not a proxy for the real host: it is a service of the EdgeR
on a per-process Unix socket, called with a fake host that is the only thing
the worker is allowed to "reach". The real destination travels in a header and
the EdgeR makes the outbound call itself. The worker's permissions stay
closed: the manifest `allowNet` (which must be declared, even `[]`, otherwise
the network would be open and the guard pointless) plus two entries the spawn
adds on its own: `unix:<guard.sock>` and `edger-egress.invalid:80`.

What the EdgeR sees: the user's key, decrypted, in the memory of the `edger`
process, at call time. It never goes to a log, disk or event. This does not
change today's trust model: TLS for `pinar.dev` already terminates before the
EdgeR (Cloudflare and Traefik), so everything the user sends to the app already
crosses the EdgeR in the clear.

## Manifest

```yaml
allowNet:                    # still mandatory; the guard does not open the network
  - ai-gateway.vercel.sh:443
egress:
  enabled: true
  ports: [443]               # default [443]; HTTPS only
  connectTimeout: 5s         # default 5s
  headersTimeout: 30s        # until the response headers arrive; default 30s
  idleTimeout: 30s           # no body bytes (SSE); default 30s
  totalTimeout: 5m           # default 5m
  maxResponseBytes: 100mb    # default 100 MiB
  maxConcurrent: 8           # simultaneous calls per worker version; default 8
secrets:                     # names; values come from the app secrets vault
  - BYOK_MASTER_KEY
```

With `egress.enabled: true` the validation refuses a manifest without
`allowNet`. With `enabled: false` (or no `egress`) the other fields are ignored.

## What the EdgeR does at spawn

In the persistent process runtime (`EDGER_JS_RUNTIME` unset or `process`), for
EACH Deno process of a version with `egress.enabled`:

1. creates `guard.sock` in the process workdir (the same temp dir as the
   dispatch socket `w.sock`; the process already has read and write there);
2. adds `unix:<guard.sock>` and `edger-egress.invalid:80` to `--allow-net`;
3. sets `EDGER_EGRESS_SOCKET=<path of guard.sock>` and
   `EDGER_EGRESS_HOST=edger-egress.invalid` in the process env;
4. hands the listener to the guard service, with the identity of the process
   (worker, version, namespace, process id) and the version's settings.

The socket, the permissions and the variables exist together or not at all.
The app may use the PRESENCE of `EDGER_EGRESS_SOCKET` as the signal that the
guard exists. Without a guard service, with a failed bind or in
`EDGER_JS_RUNTIME=bridge` (and for wasm workers) none of them appear, one
`warn` per worker and version is logged and the worker starts normally. A
manifest `env` or a secret named `EDGER_EGRESS_SOCKET` / `EDGER_EGRESS_HOST` is
dropped (case-insensitive, one filter shared by the persistent process runtime
and the bridge), so the presence cannot be faked.

The serve task of a socket belongs to the process: it is aborted when the
process ends (recycle, TTL, LRU, shutdown), which closes the listener, and the
socket file disappears with the workdir. A staged version gets the guard like
any other: the spawn does not look at whether the version is the default.

There is ONE `EgressGuard` per `edger` process (built at boot from
`EgressGuardConfig::from_env()`), so `maxConcurrent` is shared by all the
processes of the same (worker, version).

## How the app calls

```ts
const egress = Deno.createHttpClient({
  proxy: { transport: "unix", path: Deno.env.get("EDGER_EGRESS_SOCKET")! },
});

export async function byokFetch(targetUrl: string, init: RequestInit = {}) {
  const headers = new Headers(init.headers);
  headers.set("x-edger-egress-url", targetUrl); // https://user-host/v1/chat/completions
  return await fetch(`http://${Deno.env.get("EDGER_EGRESS_HOST")}/`, {
    ...init,
    headers,
    client: egress,
  });
}
```

- **The request:** method, headers (including `Authorization: Bearer <user's
  key>`) and body go through as they came (body up to 8 MiB, buffered so a
  307/308 can replay it). The guard removes `x-edger-egress-url`, sets the
  destination `Host` and forwards.
- **The response:** the destination's, with status, headers and a streaming
  body. A provider 401 arrives as 401. SSE works with `res.body.getReader()`.
- **Redirects:** the guard follows up to 5 hops; at each hop it resolves DNS
  and classifies the IPs again. The app does not see the intermediate 3xx.
  `Authorization`, `Cookie` and `Proxy-Authorization` are dropped when a hop
  changes the origin.
- **Cancelling:** aborting the `fetch` (`AbortSignal`) closes the outbound
  call.

## What the guard refuses

The destination must be an absolute `https://` URL with a port in
`egress.ports`. The guard resolves the name, classifies EVERY resolved IP and
dials the one it classified (it never resolves a second time, so DNS
rebinding cannot swap the IP). One refused IP refuses the whole request. The
refused ranges (IPv4 and IPv6, including mapped addresses and decimal, octal
and hex notations, also on a redirect hop): private, loopback, link-local,
CGNAT, cloud metadata, cluster, the node IP, documentation and reserved.

A refusal carries `x-edger-egress-error: <code>` and the JSON body
`{"error": "<code>", "message": "..."}` (the message never echoes a request
header). Without that header the response is the destination's.

| Case | Status | Code |
|---|---|---|
| `x-edger-egress-url` missing or invalid, or not `https` | 400 | `EGRESS_BAD_TARGET` |
| port outside `egress.ports` | 403 | `EGRESS_PORT_BLOCKED` |
| refused IP (see above), also on a redirect hop | 403 | `EGRESS_HOST_BLOCKED` |
| DNS does not resolve | 502 | `EGRESS_DNS_ERROR` |
| TLS or connection failure at the destination | 502 | `EGRESS_UPSTREAM_ERROR` |
| more than 5 redirects | 502 | `EGRESS_TOO_MANY_REDIRECTS` |
| destination `Content-Length` above `maxResponseBytes` | 502 | `EGRESS_RESPONSE_TOO_LARGE` |
| `connectTimeout` or `headersTimeout` exceeded | 504 | `EGRESS_TIMEOUT` |
| `maxConcurrent` exceeded (no queue) | 429 | `EGRESS_LIMIT` |
| request body above 8 MiB | 413 | `EGRESS_REQUEST_TOO_LARGE` |

If a streaming body passes `maxResponseBytes`, or `idleTimeout` /
`totalTimeout` expire in the middle of it, the headers already left. The guard
then ENDS THE STREAM WITH AN ERROR: the app's `reader.read()` throws. The body
is never truncated silently.

## Registration

Every call records one `worker.egress` operational event: worker, version,
namespace, process id, outcome (`ok`, `blocked`, `error`, `timeout`, `limit`),
final status, duration and, for a refusal, the code; the destination host and
port, the connected IP, the bytes received and the redirect count ride the
event `message` as `key=value` tokens. It never records a header (neither a
name nor a value, not even `Authorization`), a body or a query string.

Per-worker counters: `edger_egress_requests_total{worker,outcome}` and
`edger_egress_response_bytes_total{worker}`. The boot registers them with
`ServerState::set_egress_metrics` and both `/metrics` renderers
(`pipeline.rs` and `server::router`) append `egress_metrics_prometheus`; with
no guard registered the `/metrics` carries no guard line.

## Operator variables

| Variable | Meaning |
|---|---|
| `EDGER_EGRESS_BLOCK_CIDRS` | comma list of extra CIDRs the guard refuses; empty keeps the k3s pod and service ranges (`10.42.0.0/16`, `10.43.0.0/16`) |
| `EDGER_NODE_IP` | the node IP, also refused (the Helm chart sets it from the downward API, `status.hostIP`) |
| `EDGER_EGRESS_ALLOW_CIDRS` | **local development only**: CIDRs the classification would refuse (such as `127.0.0.0/8`); empty by default, a `warn` at first use, NOT in the chart, never set in production |

Helm: `egress.blockCidrs` (optional list) becomes `EDGER_EGRESS_BLOCK_CIDRS`.

## App secrets

Secrets (for example `BYOK_MASTER_KEY`) live in the EdgeR vault and reach only
the env of the process of the worker that declares the name. See the
`CHANGELOG.md` entry of the app secrets vault and
`PUT|GET|DELETE /api/admin/workers/{name}/secrets[/{secret}]`.

## Proof

- `crates/edger-core` tests: IP classification and manifest validation (S1).
- `crates/edger-orchestrator/tests/egress_guard.rs`: the service, with a real
  TLS destination and a `hyper` client (S2a).
- `crates/edger-isolation/src/deno_sandbox_policy.rs` unit tests: the new
  `--allow-net` entries, and their absence without egress.
- `crates/edger-orchestrator/tests/egress_e2e.rs` (real `deno`, `--ignored`):
  the contract request, streaming, `NotCapable` on a direct fetch, a refused
  destination, no socket without `egress` (and a forged `env` ignored), a
  staged version, a recycle leaving no task and no socket, a failed bind
  leaving no socket and one `warn`, and the guard counters at `/metrics`
  after a call (plus a test without Deno: no setter, no guard line, in both
  renderers).
