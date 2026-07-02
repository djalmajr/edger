# Story 15.C: Compatibilidade de frameworks (Express, Hono)

**Origin:** `planning/edger/epics/15-runtime-js-duravel/00-overview.md`

## Context

- **Problema:** o produto exige rodar frameworks JS (Express/Hono/npm) "como num Deno local". A ponte v1 tem um adapter mínimo `node:http`; falta cobertura real de frameworks.
- **Objetivo:** Express (via `npm:express` + `node:http`) e Hono (via `npm:hono`/JSR) rodam pelo processo persistente e viram `tested` na compat-matrix.
- **Valor:** entrega o requisito de alta compatibilidade; migração de apps existentes fica trivial.
- **Restrições:** usar o Deno completo (npm/node compat nativos); não reimplementar APIs.

## Traceability

- `edger-isolation/src/deno/worker_host.rs` (captura de listener node/`Deno.serve`)
- `planning/edger/docs/compat-matrix.md`
- `workers/commonjs*`, novas fixtures `workers/express-demo`, `workers/hono-demo`

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/deno/worker_host.rs` | edit | Captura robusta de `app.listen()` (node:http) e `Deno.serve`/default fetch |
| `workers/express-demo/` | create | Fixture Express (`npm:express`) |
| `workers/hono-demo/` | create | Fixture Hono (`npm:hono`) |
| `edger-orchestrator/tests/kind_dispatch_integration.rs` | edit | E2E Express/Hono respondendo por UDS |
| `planning/edger/docs/compat-matrix.md` | edit | Linhas Express/Hono → tested |

## Detail

### TO-BE
- Harness captura o ponto de entrada do framework: `Deno.serve`, `export default { fetch }`, ou listener `node:http.createServer().listen()`; requests do orquestrador alimentam o handler capturado.
- Fixtures mínimas Express e Hono com rota JSON + rota param.

### Scope
- **In:** captura de listener, fixtures Express/Hono, E2E, compat-matrix.
- **Out:** cobertura exaustiva de todo npm; SSR full Next.js (adapter futuro).

### Acceptance criteria
- [ ] `express-demo` (`npm:express`) responde rota JSON e rota `:param` via UDS.
- [ ] `hono-demo` (`npm:hono`) idem.
- [ ] Ambos aparecem como `tested` na compat-matrix com o teste referenciado.
- [ ] Sem reimplementação de node/npm no Rust (é o Deno que resolve).

### Dependencies
- Story 15.B

## Tasks
### Fase 1 — Captura
- [ ] Robustecer captura de `app.listen()`/`Deno.serve` no harness.
### Fase 2 — Fixtures + E2E
- [ ] `workers/express-demo`, `workers/hono-demo` + testes E2E via UDS.
### Fase 3 — Doc
- [ ] compat-matrix Express/Hono tested.

## Verification

```bash
cargo test -p edger-orchestrator --test kind_dispatch_integration --features multiproc -- express hono
cargo test --workspace
```
