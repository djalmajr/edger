# edger

Edge runtime (Rust vision + Buntime contracts) with Bun adapter for immediate functionality.

## Objetivo

- Orquestrador em Rust (sem main-service TS separado).
- Extensibilidade via crates (edger-{core,worker,isolation,orchestrator,ext-*}).
- Suporte JS/TS (Deno.serve / default fetch compat), serverless, backend, SPA, fullstack/SSR, Wasm.
- Open/Closed: core fechado, extensões abertas.
- Contratos Buntime: workers, manifests, namespaces, TTL, shell.

## Current (Fase 1 functional)

- Bun-based `edger.ts` loader (portable loadWorkerHandler) + CLI.
- Supports workers/ with `index.{ts,js,mjs}` verbatim from edge-runtime/examples (Deno.serve or export default {fetch}).
- 11+ examples copied and several running:
  - hello-world, serve-declarative-style, empty-response, read-body, chunked-text, sse, stream, ...
- `bun test` passes (core compat).
- Planning: intake/roadmap/design/analysis + epics/01-fundacao (completed) + status/ + AGENTS.md .
- Rust skeleton (workspace crates) ready for Fase 2+ (core pure vocab first).

## Como rodar (Bun adapter)

```bash
bun edger.ts --dir workers/hello-world --port 8000
# in another term:
curl -X POST -H 'content-type: application/json' -d '{"name":"Test"}' http://localhost:8000/
```

Worker dirs must follow the deno.server pattern (index compat).

## Status

Fase 1 complete (loader + examples + discipline + docs). Ver `planning/edger/roadmap.md`, `planning/edger/status/closure-2026-06-28-edger-func.md`, `planning/edger/epics/01-fundacao/`.

Next: `/agile-epic` for Fase 2 (edger-core).

## Rust (future)

`cargo build` / workspace. Embedding spike in later Fase.

See planning/edger/* and AGENTS.md .
