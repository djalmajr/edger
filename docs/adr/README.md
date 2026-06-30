# ADRs — Architecture Decision Records (Edger)

Índice das decisões arquiteturais do **Edger**. Comece por aqui para entender
por que o runtime tem esse formato, não apenas como ele funciona.

## O que é um ADR

Um ADR registra uma decisão estrutural: contexto, decisão tomada e
consequências. ADR aceito não deve ser reescrito para refletir uma nova decisão;
quando a arquitetura mudar, crie um ADR novo que supersede o anterior.

## Convenções

- Numeração sequencial com quatro dígitos: `0001`, `0002`, ...
- Nome do arquivo: `NNNN-titulo-em-kebab-case.md`.
- Status possíveis: `Proposto`, `Aceito`, `Superado por NNNN`, `Descontinuado`.
- Escopo: arquitetura, fronteiras, segurança, runtime, manifests, extensões e
  contratos de worker.

## Índice de ADRs

| # | Título | Status |
|---|---|---|
| [0001](./0001-orquestrador-rust-sem-main-service-ts.md) | Orquestrador Rust sem main-service TS/Deno | Aceito |
| [0002](./0002-core-puro-e-crates-focadas.md) | `edger-core` puro e crates focadas | Aceito |
| [0003](./0003-extensoes-estaticas-via-crates.md) | Extensões estáticas via crates `edger-ext-*` | Aceito |
| [0004](./0004-js-ts-v1-via-deno-cli-bridge.md) | JS/TS v1 via Deno CLI bridge, com `deno_core` como alvo | Aceito |
| [0005](./0005-wasm-standalone-wasmtime-abi-v1.md) | Wasm standalone com `wasmtime` e ABI HTTP v1 | Aceito |

## Template

```markdown
# ADR NNNN — Título

- **Status:** Proposto | Aceito | Superado por NNNN | Descontinuado
- **Data:** AAAA-MM-DD

## Contexto

O que motivou a decisão? Quais alternativas foram consideradas?

## Decisão

A decisão tomada, em voz ativa.

## Consequências

Trade-offs, custos e benefícios.

## Status

Histórico de transição do ADR.
```

## Documentos irmãos

- [Documentação geral](../README.adoc)
- [Developers — Arquitetura](../developers/01-arquitetura.adoc)
- [Developers — Segurança e isolamento](../developers/04-seguranca-e-isolamento.adoc)
