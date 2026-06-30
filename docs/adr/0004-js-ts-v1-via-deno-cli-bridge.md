# ADR 0004 — JS/TS v1 via Deno CLI bridge, com `deno_core` como alvo

- **Status:** Aceito
- **Data:** 2026-06-29

## Contexto

O objetivo do Edger é executar JS/TS pelo pipeline Rust, sem voltar ao adapter
Bun. O alvo de produção é `deno_core` embutido, mas o boot de V8/facade tem
risco técnico e não deve bloquear a prova funcional do contrato externo.

Alternativas consideradas:

- manter o adapter Bun como fallback;
- esperar `deno_core` embutido antes de validar workers reais;
- usar Deno CLI como ponte funcional temporária.

O adapter Bun foi removido do caminho ativo. Esperar `deno_core` deixaria o
runtime sem vertical slice real por mais tempo.

## Decisão

Usar Deno CLI bridge v1 para executar JS/TS real pelo pipeline Rust:

- `DenoIsolate` chama `deno eval --no-check`;
- o processo roda no diretório do worker;
- `deno.json` / `deno.jsonc` local é carregado quando existe;
- suporta `Deno.serve(handler)` e `export default { fetch }`;
- converte `SerializedRequest` e `SerializedResponse` por JSON;
- aplica timeout por request.

`deno_core` embutido continua sendo o alvo de produção.

## Consequências

Positivas:

- MVP funcional sem reintroduzir Bun;
- testes E2E exercem JS/TS real;
- contratos de request/response ficam validados cedo;
- permite validar fixtures `workers/` pelo servidor Rust.

Custos:

- processo por request não é alvo de performance;
- permissões/cache/rede do Deno CLI ainda precisam de hardening;
- streaming real ainda não é passthrough;
- `deno` precisa estar no `PATH` ou em `EDGER_DENO_BIN`.

## Status

Aceito em 2026-06-29. Fonte de verdade: `edger-isolation/src/deno/cli.rs`,
`edger-orchestrator/tests/kind_dispatch_integration.rs` e
`planning/edger/status/checkpoint-2026-06-29-story-07-04-deno-cli-bridge.md`.
