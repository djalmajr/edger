# Story 15.C: Compatibilidade de frameworks (Express, Hono)

**Origin:** `planning/edger/epics/15-runtime-js-duravel/00-overview.md`

## Context

- **Problema:** o produto exige rodar frameworks JS (Express/Hono/npm) "como num Deno local". A ponte v1 tem um adapter mínimo `node:http`; falta cobertura real de frameworks.
- **Objetivo:** Express (via `npm:express` + `node:http`) e Hono (via `npm:hono`/JSR) rodam pelo processo persistente e viram `tested` na compat-matrix.
- **Valor:** entrega o requisito de alta compatibilidade; migração de apps existentes fica trivial.
- **Restrições:** usar o Deno completo (npm/node compat nativos); não reimplementar APIs.

## Traceability

- `crates/edger-isolation/src/deno/worker_host.rs` (captura de listener node/`Deno.serve`)
- `planning/edger/docs/compat-matrix.md`
- `workers/examples/commonjs*`, novas fixtures `workers/examples/express-demo`, `workers/examples/hono-demo`

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/deno/worker_host.rs` | edit | Captura robusta de `app.listen()` (node:http) e `Deno.serve`/default fetch |
| `workers/examples/express-demo/` | create | Fixture Express (`npm:express`) |
| `workers/examples/hono-demo/` | create | Fixture Hono (`npm:hono`) |
| `crates/edger-orchestrator/tests/kind_dispatch_integration.rs` | edit | E2E Express/Hono respondendo por UDS |
| `planning/edger/docs/compat-matrix.md` | edit | Linhas Express/Hono → tested |

## Detail

### TO-BE
- Harness captura o ponto de entrada do framework: `Deno.serve`, `export default { fetch }`, ou listener `node:http.createServer().listen()`; requests do orquestrador alimentam o handler capturado.
- Fixtures mínimas Express e Hono com rota JSON + rota param.

### Scope
- **In:** captura de listener, fixtures Express/Hono, E2E, compat-matrix.
- **Out:** cobertura exaustiva de todo npm; SSR full Next.js (adapter futuro).

### Acceptance criteria
- [x] `express-demo` (`npm:express`) responde rota JSON e `:param` via processo (captura de `app.listen()`).
- [x] `hono-demo` (`npm:hono`) idem (via `Deno.serve(app.fetch)`).
- [x] Express e Hono → `tested` na compat-matrix, referenciando `framework_compat.rs`.
- [x] Zero reimplementação de node/npm no Rust — o Deno completo resolve `npm:`; harness só captura o listener.

### Dependencies
- Story 15.B

## Tasks
### Fase 1 — Captura
- [x] Adapter `node:http` (createServer/listen + node req/res) portado para o harness persistente.
### Fase 2 — Fixtures + E2E
- [x] `workers/examples/express-demo`, `workers/examples/hono-demo` + E2E `framework_compat.rs` (validado ao vivo).
### Fase 3 — Doc
- [x] compat-matrix Express/Hono tested.

## Verification

```bash
cargo test -p edger-orchestrator --test kind_dispatch_integration --features multiproc -- express hono
cargo test --workspace
```

## Status

**completed** (2026-07-02) — Express (`npm:express@5`) e Hono (`npm:hono@4`)
rodam pelo processo Deno persistente sem reimplementação: o harness ganhou o
adapter `node:http` (captura de `http.createServer(app).listen()` + node
req/res) portado da ponte v1, além da captura `Deno.serve` já existente. Sandbox
do worker liberou leitura do cache Deno (`DENO_DIR`/default) para resolver `npm:`
e `--allow-sys`. Fixtures `workers/examples/express-demo` e `workers/examples/hono-demo`; E2E
`framework_compat.rs` (ignored: precisa deno+npm), validado ao vivo pelo servidor
(`{"framework":"express"}`, `{"user":"5"}`, `{"framework":"hono"}`). compat-matrix
com Express/Hono `tested`.
