# ADR 0001 — Orquestrador Rust sem main-service TS/Deno

- **Status:** Aceito
- **Data:** 2026-06-29

## Contexto

O Edger herda a visão do Buntime: workers, manifests, namespaces, hooks,
lifecycle, shell e extensibilidade. No Buntime, boa parte da orquestração vive
em uma camada TypeScript/Bun, o que acelera protótipos, mas limita controle
fino de isolamento, CPU, memória, lifecycle e políticas.

Uma alternativa seria preservar um "main-service" TS/Deno para routing,
auth e hooks, usando Rust apenas para partes de isolamento. Isso reintroduziria
a mesma fronteira que o projeto quer remover.

## Decisão

Implementar a orquestração principal em Rust. O binário `edger` deve ser a
entrada funcional do runtime e deve compor:

- HTTP server;
- auth gate;
- route resolver;
- manifest loader;
- extension registry;
- request pipeline;
- worker pool;
- isolamento JS/TS e Wasm.

Código JS/TS continua sendo workload suportado, mas não governa a orquestração
do runtime.

## Consequências

Positivas:

- fronteiras de responsabilidade mais claras;
- política de auth, routing e lifecycle controlada pelo runtime;
- base melhor para supervisão, métricas e limites;
- migração gradual de contratos Buntime sem carregar o adapter Bun.

Custos:

- mais trabalho inicial em Rust para contratos que eram simples em userland TS;
- compatibilidade JS/TS depende de backend dedicado;
- mudanças em manifests e pipeline exigem testes de integração fortes.

## Status

Aceito em 2026-06-29. Fonte de verdade de implementação: `edger-orchestrator/`,
`edger-worker/`, `edger-isolation/`, `edger-core/` e `README.md`.
