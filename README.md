# edger

Edge runtime in Rust with Buntime-compatible contracts.

## Objetivo

- Orquestrador em Rust (sem main-service TS separado).
- Extensibilidade via crates (edger-{core,worker,isolation,orchestrator,ext-*}).
- Suporte JS/TS (Deno.serve / default fetch compat), serverless, backend, SPA, fullstack/SSR, Wasm.
- Open/Closed: core fechado, extensões abertas.
- Contratos Buntime: workers, manifests, namespaces, TTL, shell.

## Current

- Rust orchestrator loads worker roots from `RUNTIME_WORKER_DIRS` (default `workers`) and serves the pipeline.
- `workers/wasm-hello` executes through the Rust/wasmtime path.
- JS/TS workers execute through the Rust pipeline using the Deno CLI bridge in `edger-isolation`.
- The Deno bridge runs from the worker directory and loads local `deno.json` / `deno.jsonc` when present.
- The historical Bun adapter was removed; the Rust binary is the only runtime entrypoint.
- Planning: intake/roadmap/design/analysis + **9 epics / 57 numbered stories + 1 spike artifact** (`planning/edger/epics/`) + status/ + AGENTS.md.
- Fases 1-6 delivered as foundation; Fase 7 remains the advanced-runtime track, Fase 8 now has value-parity proof plus follow-up slices for must-have operational gaps, and Fase 9 has started durable external provider planning.

## Como rodar

```bash
ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
```

Em outro terminal:

```bash
curl -H 'authorization: Bearer test-root' http://127.0.0.1:19080/wasm-hello
curl -H 'authorization: Bearer test-root' -H 'content-type: application/json' \
  -d '{"name":"Alice"}' http://127.0.0.1:19080/hello-world
```

The JS/TS backend requires `deno` on `PATH` or `EDGER_DENO_BIN=/path/to/deno`. Embedded `deno_core` remains the planned production optimization.

## Status

Foundation complete through Fase 6. Fase 7 is in progress: manifest loading, Wasm v1, Deno/fetch JS/TS examples, static SPAs, shell routing, state bindings, operation probes and value-parity checks now execute through the Rust server. `stream`/`sse` use a bounded first-chunk fallback until true streaming passthrough lands; CommonJS server-listen has a minimal Node adapter and mounted workers follow Buntime path semantics: relative path plus `x-base`.

Fase 8 has executable value evidence for SPA `/todos`, manifest-less `index.html` autodiscovery, protected workers, state bindings, shell/gateway routing, gateway CORS/auth behavior, gateway redirect rules with query preservation, gateway local rate limiting, gateway operational diagnostics, gateway read-only Admin API for stats/config/logs/log-stats with duration and rate-limit metrics, pool metrics, worker stats, API key create/revoke, Deno manifest env filtering, runtime worker enable/disable, runtime extension enable/disable, and the Buntime-derived `base: ""` route-hijack guard. Remaining explicit gaps are embedded `deno_core`, native cron execution, proxy/cache/rate-limit persistence and distribution, gateway SSE/history/mutations, retry/DLQ depth, persistent extension reload and marketplace. Turso remote/sync is now tracked as a durable external provider in Fase 9, not as an internal edger implementation requirement.

Ver `planning/edger/roadmap.md`, `planning/edger/runtime-functional-plan.md`, `planning/edger/docs/pendencies-epic-07.md`, `planning/edger/docs/value-parity-matrix.md`.

Next: continue Epic 07 hardening where gaps depend on runtime foundation, and use Epic 09 for durable external provider work such as Turso remote/sync. Keep `planning/edger/docs/value-parity-matrix.md` as the source of truth for remaining Buntime-value gaps.

## Gates

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

See planning/edger/* and AGENTS.md .
