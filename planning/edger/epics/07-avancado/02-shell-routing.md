# Story 07.02: Shell routing e injeção de base (micro-frontends)

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`

## Context
- **Problema:** Workers SPA e micro-frontends Buntime dependem de `<base href>` injetado e rotas shell dedicadas; o orquestrador atual não distingue shell vs API worker nem documenta evolução de protocolo além de MessageChannel/z-frame.
- **Objetivo:** Implementar decisão de shell routing no orchestrator com `inject_base`, preservando compat Buntime e registrando notas para protocolo evoluído (WebTransport etc.).
- **Valor:** UIs embeddáveis funcionam sob paths namespaced sem quebrar assets relativos; caminho de migração para shell moderno fica explícito.
- **Restrições:** Base injection também ocorre no isolate (`serve_static_spa`); shell routing é camada orchestrator; não implementar WebTransport completo em v1 — documentar contrato futuro.

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 11, Shell/Micro-frontends, Resolved Decisions)
- **Design PR:** PR 11
- **Buntime refs:** `planning/edger/design.md (shell; ai-memory zommehq/buntime)` (z-frame, base injection), `injectBase` em manifests
- **Depende de:** `01-full-manifests-kinds.md` (WorkerRef + StaticSpa kind)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/shell.rs` | create | Lógica de shell routing, flags UI, rewrite de paths |
| `edger-orchestrator/src/router.rs` | edit | Rotas reservadas shell vs worker; homepage fallback |
| `edger-orchestrator/src/pipeline.rs` | edit | Branch shell antes/depois de worker dispatch conforme regra |
| `edger-isolation/src/isolate.rs` | edit | `serve_static_spa` com injeção `<base href>` quando `inject_base` |
| `edger-core/src/wire.rs` | edit | Campo `base_href` em `SerializedRequest` preenchido pelo orchestrator |
| `edger-orchestrator/tests/shell_routing_test.rs` | create | SPA sob `/@scope/app`, assets relativos, base correto |
| `workers/shell-spa/` | create | Fixture HTML + manifest `inject_base: true` |
| `planning/edger/design.md` ou `planning/edger/docs/shell-protocol.md` | edit/create | Notas de protocolo evoluído (WebTransport, preservação z-frame compat) |

## Detail

### AS-IS
- `inject_base` pode existir no manifest sem wiring no router.
- Sem módulo `shell.rs`; micro-frontend tratado como request estático genérico.
- Protocolo iframe/MessageChannel não documentado no edger.

### TO-BE
- Router identifica workers “UI/shell” (manifest `visibility` + kind SPA ou flag explícita).
- Orchestrator calcula `base_href` (equivalente Buntime `X-Base`) e injeta em `SerializedRequest` + HTML estático.
- Paths de asset relativos (`./main.js`) resolvem corretamente sob prefixo namespaced.
- Documento descreve: (a) comportamento compat z-frame; (b) direção WebTransport; (c) headers/credenciais internas para shell API.
- Reserved paths (`/api`, `/health`, `/.well-known`) não interceptados pelo shell.

### Scope
- **In:** Shell routing decision, base href injection, testes SPA namespaced, doc protocolo evoluído.
- **Out:** Implementação WebTransport real; cpanel; marketplace UI plugins.

### Acceptance criteria
- [ ] Worker SPA com `inject_base: true` retorna HTML contendo `<base href="...">` correto para path namespaced.
- [ ] Asset relativo referenciado no HTML é servível via mesma worker route (teste integração).
- [ ] Shell não captura `/api/*` nem health endpoints.
- [ ] `base_href` propagado em `SerializedRequest` para handlers que precisam gerar URLs absolutas.
- [ ] Documento `planning/edger/docs/shell-protocol.md` (ou seção design) descreve compat atual + evolução planejada.
- [ ] Com `inject_base: false`, HTML servido sem modificação de `<base>`.

### Dependencies
- Story 07.01 (manifests + StaticSpa dispatch)
- Story 07.04 (serve_static_spa real ou mock com injeção HTML)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`edger-orchestrator/tests/`, `edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Base href pipeline
- [ ] Implementar cálculo de `base_href` no orchestrator (port lógica Buntime path + scope).
- [ ] Preencher `SerializedRequest.base_href` antes do dispatch.
- [ ] Teste unitário: paths `/@acme/checkout`, `/checkout`, plugin `base` precedence.

### Fase 2 — Shell module
- [ ] Criar `shell.rs` com regras de quando aplicar shell wrapper vs dispatch direto.
- [ ] Integrar no router: homepage fallback, worker UI flags.
- [ ] Conectar `serve_static_spa` no isolate com injeção condicional.

### Fase 3 — Integração e documentação
- [ ] Fixture `workers/shell-spa/` + teste E2E.
- [ ] Escrever `planning/edger/docs/shell-protocol.md`: z-frame compat, MessageChannel legado, roadmap WebTransport.
- [ ] Cross-ref em `00-overview.md` epic status.

### Fase 4 — Verificação
- [ ] `cargo test -p edger-orchestrator -- shell`
- [ ] Gate workspace completo.

## Verification
- `cargo test -p edger-orchestrator -- shell_routing`
- `cargo test -p edger-isolation -- static_spa` (se módulo separado)
- Manual: `curl` em worker SPA namespaced — inspecionar `<base href>` no body
- `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check`
- `bun test`