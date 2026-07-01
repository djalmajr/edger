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
  - Sem deploy remoto nesta fase; validação deve ser local, usando `docker-compose` quando uma dependência local for necessária.
  - CPanel/admin UI, shell, MCP, gateway avançado, templates e authoring deixam de ser "pós-escopo genérico" e passam a ser módulos/epics próprios.

## Roadmap objectives
- Objetivo 1: Ter um skeleton + estrutura de crates alinhada, com disciplina de engenharia e testes desde o dia 1 (base para tudo).
- Objetivo 2: Definir e implementar o núcleo de domínio/tipos/traits de forma pura e reutilizável (core como vocabulário).
- Objetivo 3: Entregar camada de isolamento/execução básica (com spike de embedding) + gerenciamento de workers confiável.
- Objetivo 4: Orquestrador funcional em Rust (roteamento, auth, hooks, servidor básico) preservando contratos Buntime.
- Objetivo 5: Mecanismo de extensões via crates + primeiras extensões úteis.
- Objetivo 6: Features avançadas (manifests completos, shell, Wasm, observabilidade) + preparação para migração e uso real.
- Objetivo 7: Providers duráveis externos substituíveis, mantendo Turso remoto/sync fora do core/orchestrator e atrás de contratos estáveis.
- Objetivo 8: Modularizar capacidades de produto aprendidas no Buntime em epics próprios, sem acumular tudo no Epic 8 nem no core.
- Objetivo 9: Entregar uma primeira versão funcional, testada localmente, de control plane MCP/AI-native para agentes descobrirem, criarem ou modificarem workers, validarem e prepararem commits/PRs sem deploy remoto.

## Initiatives / Epics
| Initiative | Epic | Stories | Status | Dependency |
|---|---|---|---|---|
| Fase 1: Fundação | [`epics/01-fundacao/`](epics/01-fundacao/00-overview.md) | 4 | **completed** (historical Bun bootstrap removed from active runtime) | -- |
| Fase 2: edger-core | [`epics/02-edger-core/`](epics/02-edger-core/00-overview.md) | 4 | **completed** | Fase 1 |
| Fase 3: Isolação + Spike | [`epics/03-isolacao-execucao/`](epics/03-isolacao-execucao/00-overview.md) | 4 | **completed** | Fase 2 |
| Fase 4: Worker Management | [`epics/04-worker-management/`](epics/04-worker-management/00-overview.md) | 4 | **completed** | Fase 2, Fase 3 |
| Fase 5: Orquestrador | [`epics/05-orquestrador/`](epics/05-orquestrador/00-overview.md) | 5 | **completed** | Fase 1-4 |
| Fase 6: Extensibilidade | [`epics/06-extensibilidade/`](epics/06-extensibilidade/00-overview.md) | 3 | **completed** | Fase 5 |
| Fase 7: Avançado | [`epics/07-avancado/`](epics/07-avancado/00-overview.md) | 7 | in-progress | Fase 5-6 |
| Fase 8: Valor Buntime | [`epics/08-valor-buntime/`](epics/08-valor-buntime/00-overview.md) | 29 | **completed as consolidation** (matriz/paridade observável; novas capacidades seguem em epics próprios) | Fase 7 baseline |
| Fase 9: Providers Duráveis Externos | [`epics/09-providers-duraveis-externos/`](epics/09-providers-duraveis-externos/00-overview.md) | 5 | in-progress (provider crate, wiring and consumer evidence delivered; real Turso target remains opt-in) | Fase 8.04, Fase 8.06 |
| Fase 10: Operação de Extensões e Plugins | [`epics/10-operacao-extensoes-plugins/`](epics/10-operacao-extensoes-plugins/00-overview.md) | 4 | **completed** | Fase 6, Fase 8.13, Fase 8.26 |
| Fase 11: Gateway Operacional Avançado | [`epics/11-gateway-operacional-avancado/`](epics/11-gateway-operacional-avancado/00-overview.md) | 4 | in-progress (11.01 local loopback proxy delivered) | Fase 8.15-8.21, Fase 9 |
| Fase 12: Frontends Modulares e cPanel | [`epics/12-frontends-modulares-cpanel/`](epics/12-frontends-modulares-cpanel/00-overview.md) | 4 | in-progress (minimum cPanel worker + Browser validation delivered) | Fase 8.05, Fase 10, Fase 11 |
| Fase 13: MCP e Authoring AI-native Local | [`epics/13-mcp-authoring-ai-native/`](epics/13-mcp-authoring-ai-native/00-overview.md) | 5 | **completed** | Fase 8, Fase 10, Fase 12 |

## Suggested sequence
1. Fase 1 (Fundação) -- Alinha o skeleton real e estabelece cultura (AGENTS, testes, gate). Alta prioridade porque desbloqueia tudo e evita dívida técnica.
2. Fase 2 (Core) -- Paralelo parcial com Fase 1 no final; core é pré-requisito para quase tudo. Foco em pureza (sem I/O) como em ai-memory-core.
3. Fase 3 (Isolação + Spike) -- Depende de core. Fazer o spike cedo para validar embedding (decisão usuário: deno_core + facade).
4. Fase 4 (Worker) -- Constrói sobre core + isolação. Testes de pool com mocks.
5. Fase 5 (Orquestrador) -- Integra tudo anterior. Entrega valor visível (servidor básico rodando workers).
6. Fase 6 (Extensões) -- Paralela possível com partes de Fase 5; demonstra Open/Closed.
7. Fase 7 (Avançado) -- Consolida e prepara produção/migração. Inclui learnings de ai-memory em observabilidade e disciplina.
8. Fase 8 (Valor Buntime) -- Consolida aprendizados Buntime em matriz de valor edger-native, provas executáveis e decisões de escopo. Não recebe novas features grandes.
9. Fase 9 (Providers Duráveis Externos) -- Move Turso remoto/sync e storage compartilhado para providers substituíveis, sem contaminar o core nem o orchestrator.
10. Fase 10 (Operação de Extensões e Plugins) -- Assume reload/reconcile, manifesto operacional, diagnostics e ciclo de vida de extensões/plugins.
11. Fase 11 (Gateway Operacional Avançado) -- Assume proxy externo, cache, vhosts, rate limit persistente/distribuído e histórico/SSE local.
12. Fase 12 (Frontends Modulares e cPanel) -- Assume cPanel/admin UI, shell, catálogo de módulos, packaging de frontends e E2E local.
13. Fase 13 (MCP e Authoring AI-native Local) -- Assume MCP/tools, contratos machine-readable e fluxo funcional local para agentes criarem/modificarem workers e prepararem commits/PRs.

Paralelismo possível: Após Fase 1-2, algumas partes de worker e orquestrador podem avançar com mocks. Extensões podem começar protótipos cedo.

## Risks and dependencies
- **Embedding risk (alto)**: Spike (Fase 3) pode revelar complexidade maior (cold starts, Node subset, Wasm interop). Mitigação: spike time-boxed, fallback para mocks mais tempo.
- **Fidelidade de contratos Buntime**: Risco de drift durante tradução Rust. Mitigação: matriz explícita + testes de compat em Fase 7. PRs devem referenciar design.md.
- **Dependências externas**: Escolha final de embedding (deno_core) pode ter manutenção (versões V8). Mitigação: documentar em Risks do design.
- **Provider remoto/sync**: Risco de acoplar edger a Turso/libSQL específico. Mitigação: Epic 09 mantém `DurableSqlProvider` como fronteira; `edger-ext-turso-remote` vive como crate separado e o registro no composition root fica concentrado na Story 09.04.
- **Epic gigante / core inchado**: Risco de repetir o Epic 8 e empacotar produto, frontend, provider, gateway e MCP no core. Mitigação: novas capacidades com fronteira técnica/ciclo de vida próprio viram epics e módulos dedicados; `edger-core` permanece vocabulário/contratos essenciais.
- **AI-native só no papel**: Risco de MCP ficar apenas planejado. Mitigação: Epic 13 exige uma primeira versão funcional testada localmente, sem deploy remoto, com tools para descoberta, authoring, validação e preparação de commit/PR.
- **Escopo creep**: Querer tudo (full SSR Next.js nativo) cedo. Mitigação: non-goals claros no intake/design; foco em foundation + adapters.
- **Testes e disciplina**: Sem gate forte desde início, qualidade cai. Mitigação: Fase 1 entrega o gate obrigatório.
- **Migração**: Usuários Buntime existentes. Mitigação: preservar contracts (fetch/routes, manifests, namespaces, TTL, shell) + docs de mapping.
- Dependências chave: Fase 3 depende de decisões do spike; Fase 5 depende de worker+isolation; extensões dependem de registry no orquestrador.

## Out of commitment
- Implementação completa de todos plugins Buntime atuais (serão edger-ext-* em fases futuras).
- Deploy remoto/K8s/Helm nesta fase; validação deve ser local e `docker-compose` é permitido para dependências locais.
- Marketplace completo.
- 100% compat Node/full Next.js sem adapters (documentar tiers).
- Dynamic loading de crates Rust em runtime (estático primeiro).
- Performance numbers finais (definir baselines em Fase 7).
- Multi-proc clustering full (começar early, mas full depois).

## Verification
- [x] Fase 1 (Fundação) complete as historical bootstrap; active runtime path is now Rust-only (`edger.ts` removed).
- [x] Roadmap objectives são observáveis (servidor `edger` + pipeline + auth ext após Fase 5–6).
- [x] Sequência faz sentido com dependências (core antes de tudo, spike cedo).
- [x] Link claro entre roadmap, design PRs e futuros epics/stories.
- [x] Incorpora learnings de ai-memory (testes, core puro, AGENTS, single-actor patterns) e visão Buntime + estrutura Edge.
- [x] Stakeholder-ready: mostra jornada completa com riscos e out-of-scope; Fase 8 prova valor executável e mantém lacunas futuras explícitas.

## Recommended next step
- Fases 1–6 **delivered**. Ver `status/checkpoint-2026-06-29-epic-06-closure.md`.
- Fase 8 tem **prova executável de paridade de valor** e agora está fechada como consolidação/matriz; ver `status/checkpoint-2026-06-29-epic-08-value-parity.md`.
- Fase 9 entregou provider externo, wiring configurável e provas de consumidores reais; ver `epics/09-providers-duraveis-externos/00-overview.md`.
- **Próxima execução técnica:** continuar Fase 11, Fase 12 e Fase 7 em stories pequenas. Fase 10 está entregue; Fase 11 já tem proxy local loopback em 11.01; Fase 12 já tem cPanel/admin UI mínimo validado no Browser, mas shell/catalog e frontends derivados de contributions seguem abertos. Fase 13 está entregue como MCP local funcional.
- **Próxima estrutura de valor:** cada lacuna `must partial` ou módulo de produto deve ter epic dono claro; se crescer além de uma fatia coesa, criar novo epic em vez de continuar somando stories ao mesmo epic.
- **Plano ativo:** `runtime-functional-plan.md`.
- Per-story: `/agile-status` checkpoint + `/agile-refinement` após cada story (evidence em `status/`).

---

**Notas de integração:**
- Este roadmap consolida o intake (problema/objetivo), design.md (PRs agrupados em fases + ownership), analysis-synthesis.md (ai-memory patterns: core vocabulary, testes em `tests/`, gate completo, docs, single-writer, extensibilidade via crates dedicadas) e decisões de usuário (embedding, etc.).
- Cada fase/epic deve referenciar o design.md para detalhes técnicos.
- Atualizar este roadmap conforme o spike de embedding (Fase 3) der feedback.
- Manter alinhado com regras do buntime (test before complete, deixar mais limpo) + ai-memory (small changes, regression tests, preserve boundaries).

<!-- Save to: planning/edger/roadmap.md -->
