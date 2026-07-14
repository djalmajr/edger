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

- `DenoIsolate` chama `deno run --no-check --no-prompt` sobre um script de
  bridge efêmero escrito no diretório do worker (atualização 2026-07-02;
  originalmente `deno eval`, que concede permissão total);
- sandbox por flags explícitas: `--allow-read=<worker_dir>`, `--allow-env`
  sobre env limpo/filtrado e `--allow-net` configurável via
  `EDGER_DENO_ALLOW_NET` (`false`/`0`/`none` nega; lista de hosts restringe);
  write/run/ffi/sys ficam negados;
- o processo roda no diretório do worker;
- `deno.json` / `deno.jsonc` local é carregado quando existe;
- suporta `Deno.serve(handler)`, `export default { fetch }` e `routes` export
  (estilo Bun.serve: exact > `:param` > `*`, method map com 405, fallback
  `fetch`, 404 sem fallback);
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

Aceito em 2026-06-29. Fonte de verdade: `crates/edger-isolation/src/deno/cli.rs`,
`crates/edger-orchestrator/tests/kind_dispatch_integration.rs` e
`planning/edger/status/checkpoint-2026-06-29-story-07-04-deno-cli-bridge.md`.
