# EdgeR

EdgeR is a source-available, self-hosted runtime for JavaScript, TypeScript and
WebAssembly workers. A Rust control plane manages versioned workers, process
pools, routing, limits, deploys and local-first observability.

> **Project status:** active development. The first release under the current
> license is planned as `v0.2.0`; the `main` branch is not yet a stability
> guarantee.

## What EdgeR provides

- Versioned workers, with one default version and addressable version paths.
- Persistent Deno processes over Unix sockets for warm JS/TS execution.
- `Deno.serve`, default `fetch`, routes tables, CommonJS compatibility,
  static SPAs, full-stack SSR adapters and Wasm.
- Bounded process pools, queues, request limits, lifecycle hooks and graceful
  drain.
- Root-key or optional OIDC protection for the control plane. Worker data-plane
  authentication remains the responsibility of each worker or an external API
  gateway.
- A built-in cPanel for worker inventory, files, routing, health, logs and
  observability.
- A bundled WebIDE with local drafts, explicit deploy, sandboxed preview and
  direct access to deployments, logs and observability.
- Local in-memory operational events without requiring an external collector.
- Optional OTLP export and Prometheus-compatible metrics.
- Helm/Rancher deployment assets, including `questions.yaml`.

## Quick start

Requirements:

- Rust toolchain compatible with the workspace `rust-version`.
- Deno on `PATH`, or `EDGER_DENO_BIN` pointing to the binary.

Start the runtime with a development-only control-plane key:

```bash
ROOT_API_KEY=dev-only-change-me \
PORT=19080 \
RUNTIME_WORKER_DIRS=workers/examples \
EDGER_CORE_WORKER_DIR=workers/core \
EDGER_CORE_WORKER_OVERLAY_DIR=.edger/core-worker-overlays \
cargo run -p edger-orchestrator --bin edger
```

Then verify the runtime and open the cPanel:

```bash
curl http://127.0.0.1:19080/health
curl http://127.0.0.1:19080/ready
open http://127.0.0.1:19080/cpanel/
open http://127.0.0.1:19080/webide/
```

Worker routes are not protected by the root key. For example:

```bash
curl http://127.0.0.1:19080/wasm-hello
curl -H 'content-type: application/json' \
  -d '{"name":"Alice"}' \
  http://127.0.0.1:19080/hello-world
```

Never reuse the development key in a shared or production environment. See the
[security policy](SECURITY.md) and the
[Kubernetes deployment guide](planning/edger/docs/deployment-k8s.md).

## Architecture

The workspace is intentionally split into explicit boundaries:

- `crates/edger-core`: pure runtime vocabulary and contracts, without I/O.
- `crates/edger-isolation`: Deno process and Wasm execution backends.
- `crates/edger-worker`: process pools, lifecycle and worker metrics.
- `crates/edger-orchestrator`: HTTP server, routing, control plane, deploy and
  observability.
- `crates/edger-mcp`: local authoring and discovery surface for MCP clients.

Workers are separated by trust boundary. `workers/core` contains the cPanel and
WebIDE distributed by EdgeR; `workers/examples` contains development examples.
At runtime, bundled core workers are immutable. Administrative updates for
reserved core names are written to the overlay directory, while ordinary
workers remain under `RUNTIME_WORKER_DIRS`.

The runtime stays minimal: API-gateway policy, fleet management, durable
cross-replica retention and commercial governance live outside the worker hot
path and integrate through stable APIs or telemetry protocols.

Start with:

- [Architecture and product design](planning/edger/design.md)
- [Compatibility matrix](planning/edger/docs/compat-matrix.md)
- [Observability](planning/edger/docs/observability.md)
- [Roadmap](planning/edger/roadmap.md)

Planning documents include historical delivery records. The compatibility
matrix and the current epic statuses are authoritative when an older story or
evidence file describes a superseded architecture.

## Deployment

The Helm chart lives at [`charts/edger`](charts/edger). Validate it locally:

```bash
helm lint charts/edger
helm template edger charts/edger
```

The container image contains only the release binary, the Deno base runtime,
the runtime-ready cPanel and the compiled WebIDE. It runs as uid `10001` and
declares separate volumes for user workers and core overlays. Persist
`/app/core-worker-overlays` to keep cPanel/WebIDE updates across pod recreation;
without that volume the bundled versions are restored naturally.

OTLP is optional. EdgeR remains operable with its bounded local event store
when no collector is configured or when export is unavailable.

## Development gates

Before opening a pull request, run:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
cargo test -p edger-orchestrator --features otel
helm lint charts/edger
planning/edger/scripts/cpanel-ui-gate.sh
```

Planning changes additionally use:

```bash
SCRATCH=/tmp/edger-planning-gates
mkdir -p "$SCRATCH"
python3 planning/edger/scripts/refinement-lint.py \
  --scope planning/edger \
  --round public-readiness >"$SCRATCH/refinement-report.txt"
SCRATCH="$SCRATCH" planning/edger/scripts/run-gates.sh
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution expectations and
[RELEASING.md](RELEASING.md) for the release checklist.

## License

EdgeR is distributed under the [O'Saasy License](LICENSE). You may study, use,
modify and redistribute the software, but you may not offer it as a competing
hosted, managed, SaaS or cloud service whose primary value is the functionality
of EdgeR itself.

Because of that use restriction, EdgeR is **source-available**, not open source
under the Open Source Initiative definition. Copies previously received under
MIT retain the rights granted by that earlier distribution.

Core security, worker operation, local logs, local observability and cPanel
functionality remain part of the Community runtime. Future commercial services
may provide external fleet management, multi-cluster operation, long-term
retention, governance, compliance, automation and support.
