# ADR 0002 — `edger-core` puro e crates focadas

- **Status:** Aceito
- **Data:** 2026-06-29

## Contexto

O projeto precisa evoluir vários eixos em paralelo: manifests, auth, routing,
pool, isolamento, extensões e compatibilidade Buntime. Sem separação rígida,
o core tenderia a acumular I/O, dependências de runtime e ciclos entre crates.

As alternativas consideradas foram:

- colocar traits e implementação no mesmo crate;
- deixar extensões dependerem do orchestrator;
- permitir `edger-core` conhecer worker/isolation/orchestrator.

Essas alternativas aumentariam acoplamento e tornariam o runtime difícil de
estender sem modificar o core.

## Decisão

Manter `edger-core` como vocabulário puro:

- tipos de manifest e config;
- principals e auth helpers puros;
- traits (`Extension`, `Middleware`, `AuthProvider`, `Isolate`, etc.);
- wire structs;
- erros e parsers sem I/O.

Crates superiores dependem do core, nunca o contrário. Crates `edger-ext-*`
dependem apenas de `edger-core`.

## Consequências

Positivas:

- dependências unidirecionais;
- extensões compilam contra contratos estáveis;
- testes do core são rápidos e determinísticos;
- o orquestrador pode trocar backends sem alterar o vocabulário.

Custos:

- alguns tipos precisam ficar mais genéricos;
- wiring acontece em crates superiores;
- mudanças de contrato exigem atenção para todas as implementações.

## Status

Aceito em 2026-06-29. Fonte de verdade: `crates/edger-core/`, `Cargo.toml` e regras em
`AGENTS.md`.
