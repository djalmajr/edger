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
- [x] Worker SPA com `inject_base: true` retorna HTML contendo `<base href="...">` correto para path namespaced. (`shell_routing_test.rs::namespaced_spa_receives_injected_base_href`)
- [x] Asset relativo referenciado no HTML é servível via mesma worker route (teste integração). (`/@team/panel/app.js` no mesmo teste)
- [x] Shell não captura `/api/*` nem health endpoints. (`shell_gateway.rs::reserved_admin_path_is_not_intercepted_by_shell`, entregue na 08.05)
- [x] `base_href` propagado em `SerializedRequest` para handlers que precisam gerar URLs absolutas. (`pipeline.rs::dispatch_worker` + `x-base`)
- [x] Documento `planning/edger/docs/shell-protocol.md` (ou seção design) descreve compat atual + evolução planejada. (seção "Evolução planejada": z-frame compat, WebTransport, base_href)
- [x] Com `inject_base: false`, HTML servido sem modificação de `<base>`. (`shell_routing_test.rs::spa_with_inject_base_false_serves_untouched_html`; exigiu fix em `infer_execution_kind` que ignorava `injectBase` com `kind: spa` explícito)

### Dependencies
- Story 07.01 (manifests + StaticSpa dispatch)
- Story 07.04 (serve_static_spa real ou mock com injeção HTML)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`edger-orchestrator/tests/`, `edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Base href pipeline
- [x] Implementar cálculo de `base_href` no orchestrator (port lógica Buntime path + scope). (`pipeline.rs::base_href`, entregue na 07.04/08.05)
- [x] Preencher `SerializedRequest.base_href` antes do dispatch. (`pipeline.rs::dispatch_worker`)
- [x] Teste unitário: paths `/@acme/checkout`, `/checkout`, plugin `base` precedence. (`kind_dispatch_integration.rs::namespaced_worker_receives_relative_path_and_base_header` + `shell_routing_test.rs`)

### Fase 2 — Shell module
- [x] Criar `shell.rs` com regras de quando aplicar shell wrapper vs dispatch direto. (entregue como `shell_gateway.rs` na 08.05; módulo separado desnecessário)
- [x] Integrar no router: homepage fallback, worker UI flags. (`shell_gateway.rs` + `shellExcludes`)
- [x] Conectar `serve_static_spa` no isolate com injeção condicional. (`edger-isolation/src/deno/mod.rs::serve_static_spa`; `injectBase: false` respeitado via fix em `infer_execution_kind`)

### Fase 3 — Integração e documentação
- [x] Fixture + teste E2E. (temp-dir em `shell_routing_test.rs`; fixtures repo `workers/shell-demo`, `workers/todos-shell-demo`)
- [x] Escrever `planning/edger/docs/shell-protocol.md`: z-frame compat, MessageChannel legado, roadmap WebTransport. (seção "Evolução planejada")
- [x] Cross-ref em `00-overview.md` epic status.

### Fase 4 — Verificação
- [x] `cargo test -p edger-orchestrator -- shell`
- [x] Gate workspace completo. (ver evidência do gate na validação final)

## Verification
```bash
cargo test -p edger-orchestrator -- shell_routing
cargo test -p edger-isolation -- static_spa
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed** (2026-07-02) — A decisão de shell routing (document vs iframe,
excludes, reserved paths, homepage) foi entregue pela Story 08.05 em
`edger-orchestrator/src/shell_gateway.rs`; esta story fechou os gaps de SPA
namespaced: base injection sob `/@scope/app`, asset relativo pela mesma rota,
`injectBase: false` respeitado (fix em `edger-core::infer_execution_kind`, que
fixava `inject_base: true` para `kind: spa` explícito) e a seção "Evolução
planejada" (z-frame compat + WebTransport) em
`planning/edger/docs/shell-protocol.md`. Módulo `shell.rs` dedicado não foi
necessário; a fixture usada nos testes é temp-dir (`shell_routing_test.rs`) e
as fixtures de shell do repo seguem `workers/shell-demo` e
`workers/todos-shell-demo`.
