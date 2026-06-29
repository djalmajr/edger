# Roadmap: edger - Runtime Edge em Rust (Trajetória de Fundação)

**Origin:** `planning/edger/intake.md`, `planning/edger/design.md`, `planning/edger/analysis-synthesis.md` (síntese de Buntime vision + Edge Runtime estrutura + aprendizados de ai-memory)

**Type:** Trajectory (multi-fase com dependências fortes)

## Context
- **Roadmap objective:** Construir uma fundação sólida para o runtime "edger" em Rust, com orquestrador nativo no core (sem camada main-service separada em TS/Deno), seguindo a visão de produto do Buntime (workers como unidades de primeira classe, manifests, extensibilidade via crates para Open/Closed, suporte a JS/TS/Node compat, serverless/fullstack/SSR, Wasm, multi-tenancy, shell) e a estrutura/engenharia do Edge Runtime (separação em crates, isolates/supervisores, limites de recursos) + melhores práticas de ai-memory (core como vocabulário puro, testes de integração fortes, disciplina de gate local, documentação canônica, padrões como single-writer actor).
- **Project/product:** edger (novo projeto em /Users/djalmajr/Developer/djalmajr/edger)
- **Horizon:** Fundação (6-9 meses estimado para base executável + migração básica), com fases sequenciadas. Não é quarterly fixo, mas trajetória para ter um runtime funcional.
- **Constraints and assumptions:**
  - Projeto novo e independente (não fork direto).
  - Híbrido: orquestrador em Rust + execução de código usuário via embedding (deno_core + facade para JS/TS per decisão do usuário; wasmtime standalone para Wasm).
  - Iniciar do skeleton existente (workspace + 4 crates com apenas Cargo.toml).
  - Preservar contratos chave do Buntime para migração (manifests, fetch/routes/SPA, namespaces/auth, TTL/ephemeral, hooks, shell).
  - Extensões como crates Rust (estático inicialmente).
  - Forte ênfase em testes, disciplina (gate cargo), e separação (core sem I/O).
  - Decisões de usuário já incorporadas (do analysis): embedding deno_core+facade, static extensions v1, partial Node, eszip+precomp, Turso auth, evolve shell, native scheduler cron, multi-process early, etc.
  - Sem foco inicial em deploy K8s, cpanel completo ou marketplace.

## Roadmap objectives
- Objetivo 1: Ter um skeleton + estrutura de crates alinhada, com disciplina de engenharia e testes desde o dia 1 (base para tudo).
- Objetivo 2: Definir e implementar o núcleo de domínio/tipos/traits de forma pura e reutilizável (core como vocabulário).
- Objetivo 3: Entregar camada de isolamento/execução básica (com spike de embedding) + gerenciamento de workers confiável.
- Objetivo 4: Orquestrador funcional em Rust (roteamento, auth, hooks, servidor básico) preservando contratos Buntime.
- Objetivo 5: Mecanismo de extensões via crates + primeiras extensões úteis.
- Objetivo 6: Features avançadas (manifests completos, shell, Wasm, observabilidade) + preparação para migração e uso real.

## Initiatives / Epics
| Initiative | Epic | Stories | Status | Dependency |
|---|---|---|---|---|
| Fase 1: Fundação | [`epics/01-fundacao/`](epics/01-fundacao/00-overview.md) | 4 | **completed** (Bun loader delivered) | -- |
| Fase 2: edger-core | [`epics/02-edger-core/`](epics/02-edger-core/00-overview.md) | 4 | **completed** | Fase 1 |
| Fase 3: Isolação + Spike | [`epics/03-isolacao-execucao/`](epics/03-isolacao-execucao/00-overview.md) | 4 | **completed** | Fase 2 |
| Fase 4: Worker Management | [`epics/04-worker-management/`](epics/04-worker-management/00-overview.md) | 4 | **completed** | Fase 2, Fase 3 |
| Fase 5: Orquestrador | [`epics/05-orquestrador/`](epics/05-orquestrador/00-overview.md) | 5 | **completed** | Fase 1-4 |
| Fase 6: Extensibilidade | [`epics/06-extensibilidade/`](epics/06-extensibilidade/00-overview.md) | 3 | **completed** | Fase 5 |
| Fase 7: Avançado | [`epics/07-avancado/`](epics/07-avancado/00-overview.md) | 7 | in-progress | Fase 5-6 |

## Suggested sequence
1. Fase 1 (Fundação) -- Alinha o skeleton real e estabelece cultura (AGENTS, testes, gate). Alta prioridade porque desbloqueia tudo e evita dívida técnica.
2. Fase 2 (Core) -- Paralelo parcial com Fase 1 no final; core é pré-requisito para quase tudo. Foco em pureza (sem I/O) como em ai-memory-core.
3. Fase 3 (Isolação + Spike) -- Depende de core. Fazer o spike cedo para validar embedding (decisão usuário: deno_core + facade).
4. Fase 4 (Worker) -- Constrói sobre core + isolação. Testes de pool com mocks.
5. Fase 5 (Orquestrador) -- Integra tudo anterior. Entrega valor visível (servidor básico rodando workers).
6. Fase 6 (Extensões) -- Paralela possível com partes de Fase 5; demonstra Open/Closed.
7. Fase 7 (Avançado) -- Consolida e prepara produção/migração. Inclui learnings de ai-memory em observabilidade e disciplina.

Paralelismo possível: Após Fase 1-2, algumas partes de worker e orquestrador podem avançar com mocks. Extensões podem começar protótipos cedo.

## Risks and dependencies
- **Embedding risk (alto)**: Spike (Fase 3) pode revelar complexidade maior (cold starts, Node subset, Wasm interop). Mitigação: spike time-boxed, fallback para mocks mais tempo.
- **Fidelidade de contratos Buntime**: Risco de drift durante tradução Rust. Mitigação: matriz explícita + testes de compat em Fase 7. PRs devem referenciar design.md.
- **Dependências externas**: Escolha final de embedding (deno_core) pode ter manutenção (versões V8). Mitigação: documentar em Risks do design.
- **Escopo creep**: Querer tudo (full SSR Next.js nativo) cedo. Mitigação: non-goals claros no intake/design; foco em foundation + adapters.
- **Testes e disciplina**: Sem gate forte desde início, qualidade cai. Mitigação: Fase 1 entrega o gate obrigatório.
- **Migração**: Usuários Buntime existentes. Mitigação: preservar contracts (fetch/routes, manifests, namespaces, TTL, shell) + docs de mapping.
- Dependências chave: Fase 3 depende de decisões do spike; Fase 5 depende de worker+isolation; extensões dependem de registry no orquestrador.

## Out of commitment
- Implementação completa de todos plugins Buntime atuais (serão edger-ext-* em fases futuras).
- Deploy/K8s/Helm, cpanel, platform completa, marketplace.
- 100% compat Node/full Next.js sem adapters (documentar tiers).
- Dynamic loading de crates Rust em runtime (estático primeiro).
- Performance numbers finais (definir baselines em Fase 7).
- Multi-proc clustering full (começar early, mas full depois).

## Verification
- [x] Fase 1 (Fundação) complete: functional Bun loader + 11+ examples with deno.server index compat running + tests + docs/lints clean.
- [x] Roadmap objectives são observáveis (servidor `edger` + pipeline + auth ext após Fase 5–6).
- [x] Sequência faz sentido com dependências (core antes de tudo, spike cedo).
- [x] Link claro entre roadmap, design PRs e futuros epics/stories.
- [x] Incorpora learnings de ai-memory (testes, core puro, AGENTS, single-actor patterns) e visão Buntime + estrutura Edge.
- [ ] Stakeholder-ready: mostra jornada completa com riscos e out-of-scope (update as Fases advance).

## Recommended next step
- Fases 1–6 **delivered**. Ver `status/checkpoint-2026-06-29-epic-06-closure.md`.
- **Próxima execução:** Epic 07 — stories `04-real-js-execution.md` + `05-wasm-execution.md` (caminho crítico PR 10).
- Per-story: `/agile-status` checkpoint + `/agile-refinement` após cada story (evidence em `status/`).

---

**Notas de integração:**
- Este roadmap consolida o intake (problema/objetivo), design.md (PRs agrupados em fases + ownership), analysis-synthesis.md (ai-memory patterns: core vocabulary, testes em `tests/`, gate completo, docs, single-writer, extensibilidade via crates dedicadas) e decisões de usuário (embedding, etc.).
- Cada fase/epic deve referenciar o design.md para detalhes técnicos.
- Atualizar este roadmap conforme o spike de embedding (Fase 3) der feedback.
- Manter alinhado com regras do buntime (test before complete, deixar mais limpo) + ai-memory (small changes, regression tests, preserve boundaries).

<!-- Save to: planning/edger/roadmap.md -->