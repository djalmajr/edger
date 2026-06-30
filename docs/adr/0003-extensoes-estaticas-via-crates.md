# ADR 0003 — Extensões estáticas via crates `edger-ext-*`

- **Status:** Aceito
- **Data:** 2026-06-29

## Contexto

O Edger precisa preservar o princípio Open/Closed do Buntime: adicionar
comportamento sem modificar o core. Em TypeScript, isso era feito com plugins
dinâmicos. Em Rust, dynamic loading de crates traz complexidade de ABI,
toolchain, segurança e deploy.

Foram consideradas opções como `inventory`, `linkme`, registro automático e
carregamento dinâmico. Para a foundation, a prioridade é previsibilidade.

## Decisão

Usar crates `edger-ext-*` com registro estático explícito no binário v1.

Regras:

- cada crate de extensão depende apenas de `edger-core`;
- cada crate escolhe um modo: `Middleware`, `AuthProvider` ou `WorkerHandler`;
- o bin `edger` chama factories explícitas, como `GatewayExtension::middleware()`;
- o registry ordena middlewares por prioridade.

## Consequências

Positivas:

- wiring visível e testável;
- sem mágica de linker ou ABI dinâmica;
- extensão não acopla ao orchestrator;
- template simples para novos crates.

Custos:

- adicionar extensão exige alterar o binário;
- marketplace/dynamic loading fica para fase futura;
- múltiplos modos no mesmo crate precisam de features exclusivas ou separação.

## Status

Aceito em 2026-06-29. Fonte de verdade: `planning/edger/docs/extensions.md`,
`edger-ext-auth/`, `edger-ext-gateway/` e `edger-orchestrator/src/bin/edger.rs`.
