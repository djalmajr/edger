# Epic 06: Extensibilidade via Crates (Fase 6)

**Origin:** `planning/edger/roadmap.md` (Fase 6), `planning/edger/design.md` (PR 8–9)

## Traceability
- **Source docs:** `planning/edger/design.md` (Extension system, Key Decision #3/#7, PR 8–9, Risks — choose ONE)
- **Roadmap phase:** Fase 6 — Extensibilidade via Crates + Exemplos
- **Depends on epic:** `planning/edger/epics/05-orquestrador/00-overview.md` (registry + pipeline + auth gate)

## Context

### Problema macro
O orchestrator tem registry e traits, mas não há crates de extensão reais nem documentação do padrão de registro estático — o princípio Open/Closed permanece teórico.

### Objetivo da iniciativa
Formalizar registro compile-time (inventory/linkme), entregar `edger-ext-auth` como primeira extensão e um template `edger-ext-gateway` demonstrando wiring e regra "choose ONE".

### Resultado esperado
Bin `edger` registra extensões via padrão escolhido; `edger-ext-auth` implementa `AuthProvider`; template gateway compila e integra ao registry; docs impedem duplicação de modos na mesma crate.

### Restrições
- Registro estático v1 (sem dlopen)
- Uma crate = uma responsabilidade (auth OU gateway OU metrics — não misturar Middleware + WorkerHandler na mesma crate sem feature flags documentadas)
- Extensões dependem apenas de `edger-core` (+ deps mínimas próprias)
- Manter árvore verde com gate cargo completo

### AS-IS
- `ExtensionRegistry` planejado no Epic 05; sem crates `edger-ext-*`
- Design menciona inventory/linkme mas sem decisão documentada no repo
- Auth inline no orchestrator (story 05.04), não como extension crate

### TO-BE
- Decisão documentada: inventory **ou** linkme **ou** registro explícito no bin (escolher UMA)
- `edger-ext-auth/`: `AuthProvider` + registro
- `edger-ext-gateway/` (template): `Middleware` exemplo + README de wiring
- Workspace `Cargo.toml` inclui members ext
- Testes provam registro + hook execution com ext real

### Fora de escopo
- Dynamic loading / hot-reload de extensões
- Marketplace ou publicação crates.io
- Todas as extensões Buntime (metrics, cpanel, etc.) — apenas auth + template
- Migrar auth store do orchestrator para ext de forma breaking sem compat (fazer wiring limpo)

## Story backlog

| Story | Arquivo | Tamanho | Status | Depende de |
|---|---|---|---|---|
| 06.01 Registro estático | `01-static-registration.md` | médio | not started | Epic 05 (05.05) |
| 06.02 edger-ext-auth | `02-edger-ext-auth.md` | grande | not started | 06.01, Epic 05 (05.04) |
| 06.03 Template extensão | `03-extension-template.md` | médio | not started | 06.01, 06.02 |

## Epic roadmap

```mermaid
flowchart LR
    S01[06.01 Static registration] --> S02[06.02 edger-ext-auth]
    S01 --> S03[06.03 Template gateway]
    S02 --> S03
```

## Epic acceptance criteria
- [ ] Padrão de registro estático escolhido e documentado (inventory/linkme/explicit — uma opção)
- [ ] Regra "choose ONE" documentada: uma crate não registra dois modos conflitantes sem features
- [ ] `edger-ext-auth` compila, implementa `AuthProvider`, testes unitários passam
- [ ] Auth extension registrada no bin `edger` e participa do pipeline (substitui ou delega gate 05.04)
- [ ] `edger-ext-gateway` (template) compila como exemplo Middleware com registro
- [ ] README ou `planning/edger/docs/extensions.md` com passo-a-passo para nova extensão
- [ ] `cargo test --workspace && cargo clippy --workspace -- -D warnings` verde
- [ ] Nenhuma extensão depende de `edger-orchestrator` (apenas core)

## Risks

| Risco | Mitigação |
|---|---|
| inventory crate unmaintained | Avaliar linkme; fallback registro explícito no bin |
| Duplicação auth orchestrator vs ext | Story 06.02 define fronteira: ext implementa trait, orchestrator só chama registry |
| Proliferacao de ext-* no workspace | Template + checklist; não adicionar ext sem story |
| Feature creep no template gateway | Template mínimo (log + pass-through), sem proxy real |

## Próximo passo recomendado
`/agile-story` em `01-static-registration.md` após conclusão do Epic 05 (registry funcional).

## Status
ready-for-planning (stories definidas; implementação bloqueada por Epic 05)