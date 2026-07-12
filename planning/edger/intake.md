# Intake: edger

**Origin:** Solicitação direta do usuário durante conversa sobre viabilidade de forkar/adaptar Edge Runtime para a proposta do Buntime. Decidido criar novo projeto independente chamado "edger" usando visão/conceitos do Buntime + estrutura de código/crates do Edge Runtime. Caminho do projeto: `<repo>`. Nomeação de crates como `edger-{core,worker,...}`.

## Context
- **Problem/opportunity:** O runtime atual do Buntime (baseado em Bun + TypeScript com Web Workers) tem limitações significativas para rodar aplicações mais completas (SSR, full-stack, frameworks pesados como Next.js ou TanStack Router de forma mais nativa). A camada de orquestração fica em userland TS (Hono + WorkerPool), o que limita controle profundo sobre isolamento, recursos e customização. Há necessidade de um orquestrador mais confiável e customizável no core.
- **Initial objective:** Criar um novo projeto de runtime chamado "edger" onde:
  - A visão e capacidades vêm do Buntime (workers como unidades, separação de responsabilidades, suporte a diferentes tipos de apps, extensibilidade).
  - A estrutura de engenharia (separação em crates, mecanismos de extensão) vem dos pontos fortes do Edge Runtime.
  - O orquestrador principal é implementado em Rust (sem camada separada de main-service em Deno/TS).
  - Suporte amplo: JS/TS (com compat Node onde possível), serverless (fetch handlers), back-end, front-end (SPAs), full-stack, SSR, e WebAssembly (Wasm).
- **Expected value signal:** Maior controle e confiabilidade no core (orquestração, validações, limites de memória/CPU). Extensibilidade via crates seguindo Open/Closed Principle. Base mais sólida para plataforma multitenant e marketplace de apps/plugins. N/A — projeto exploratório/novo foundation.
- **Constraints and assumptions:**
  - Ainda será um runtime híbrido: orquestrador em Rust + execução de código usuário em JS/TS (provavelmente via embedding de Deno ou similar) e Wasm.
  - Não busca 100% compatibilidade imediata com frameworks pesados (exige adapters).
  - Plugins/extensões atuais do Buntime (dinâmicos em TS) devem evoluir para crates Rust como mecanismo de extensão.
  - Manter princípios do Buntime: manifests?, workers isolados, auth com namespaces/roles, shell/micro-frontends, etc.
  - Novo projeto independente (não fork direto do edge-runtime nem continuação direta do buntime atual).

## Initial scope
- **Includes:**
  - Estrutura de workspace em Cargo com crates nomeados `edger-core`, `edger-worker`, `edger-orchestrator`, `edger-isolation`, `edger-ext-*` etc.
  - Orquestrador principal em Rust (roteamento, resolução de workers/extensões, políticas de execução).
  - Mecanismo de extensões via crates (traits/interfaces para plugins, handlers, middlewares — substituindo o sistema atual de plugins dinâmicos).
  - Suporte básico à execução de JS/TS e Wasm.
  - Conceitos chave do Buntime traduzidos: workers, separação core/extensão, suporte a diferentes modos de app (serverless vs full apps).
  - Documentação inicial de arquitetura e intake/roadmap.
- **Does not include:**
  - Reimplementação completa de todas features atuais do Buntime de uma vez (cpanel, platform, todos plugins existentes).
  - Execução "nativa" sem esforço de Next.js ou apps full-stack pesados.
  - Camada main-service separada em JS/TS.
  - Deploy completo ou integração com K8s/Helm no início.
  - Compatibilidade total com todo o ecossistema Node sem adapters.

## Inputs and references
- **Stakeholders:** Mantenedor do projeto (usuário), visão atual do Buntime (apps/runtime, plugins, shared).
- **Documents/links:**
  - Discussão prévia sobre viabilidade de Edge Runtime vs Buntime.
  - Código do Buntime atual (visão de produto: plugins como separação, worker pool, manifests, auth com namespaces).
  - Estrutura do Edge Runtime (Cargo workspace, crates como base/base_rt/deno_facade/http_utils, ext_* para extensões, main vs user workers, supervisor).
  - AGENTS.md e regras do repositório Buntime (testes, lint, etc. — aplicar ao novo projeto onde relevante).
- **Known technical context:**
  - Edge Runtime usa Rust + Deno embedding para isolates, worker creation via ops, main-service como ponto de entrada flexível (que será removido/integrado).
  - Buntime atual: Hono para orquestração, Bun Workers, plugin loader com manifest.yaml + onRequest hooks, WorkerPool com TTL/ephemeral.
  - Objetivo de extensibilidade via crates para seguir Open/Closed.

## Open questions (resolved — see design.md)

- [x] Breakdown dos crates — `planning/edger/design.md` Crate Ownership + roadmap Fases 1–7
- [x] Embedding JS/TS + Wasm — `design.md` Resolved Decisions + Epic 03 spike + Epic 07
- [x] Modelo de extensões — compile-time + registro estático (Epic 06); traits em core
- [x] Tipos de aplicação — `ExecutionKind` em `design.md` Data Model + Epic 07.01
- [x] Compat Node.js — fora de escopo Fase 1; spike documenta sharp edges (Epic 03)
- [x] Migração Buntime — `design.md` Migration notes + mapping table; Bun adapter Fase 1 entregue
- [x] Build/test/lint Rust — `AGENTS.md` gates + stories com `cargo test`/`clippy`
- [x] Cold-starts/bundling — Epic 07.04/07.05 + `design.md` PR 10/11

## Recommended next step
`/agile-story` em `planning/edger/epics/02-edger-core/01-setup-core-crate.md` (backlog maduro; roadmap completo).

## Verification
- [x] The problem is clear enough for the next step
- [x] Constraints and assumptions have been made explicit
- [x] The next artifact in the flow has been defined

<!-- Save to: planning/edger/intake.md -->
