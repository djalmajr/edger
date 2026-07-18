# Story 24.03: Frameworks Node de servidor

**Origin:** `planning/edger/epics/24-frameworks-deno-ssr/00-overview.md`

## Context

Depois da lista oficial de frameworks SSR do Deno, a próxima lacuna de maior
valor era o ecossistema de servidores Node estruturados. NestJS, Fastify e Koa
exigem mais do lifecycle HTTP do que a captura mínima validada com Express.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/{manifest,config}.rs` | edit | Declarar o proxy HTTP privado por worker |
| `crates/edger-isolation/src/{multiproc,multiproc_harness.mjs}` | edit | Lifecycle Node e framing de stream reutilizável |
| `crates/edger-{core,isolation,orchestrator}/tests/` | edit | Regressões de manifesto, proxy e frameworks |
| `workers/framework-tests/` | add | Pacotes fonte reais para deploy ZIP |
| `planning/edger/docs/` | edit | Contrato e matriz canônica |

## Detail

`nodeHttpProxy: true` executa o servidor `node:http` real em socket Unix privado.
O outer server remove headers hop-by-hop e `Content-Length` antes de expor um
body streamed, garantindo que Hyper consuma o frame interno de término e devolva
o processo ao pool. Sem isso, a resposta chegava 200 ao cliente, mas o processo
era classificado como desconectado e reciclado.

### Acceptance criteria

- [x] NestJS funciona com `ExpressAdapter` e `FastifyAdapter`.
- [x] DI, decorators, guard, interceptor, DTO validation e streaming são reais.
- [x] Fastify puro cobre hooks, schema, rota parametrizada e stream.
- [x] Koa cobre middleware onion, router, body parser e stream.
- [x] Os quatro pacotes instalam pela Admin API com HTTP 201.
- [x] Estado live permanece warm, healthy e sem recycle por erro.

## Tasks

- [x] Criar fixtures versionadas fora das raízes auto-carregadas.
- [x] Adicionar o contrato `nodeHttpProxy` ao manifesto normalizado.
- [x] Completar o evento `listening` na captura Node mínima.
- [x] Corrigir headers de framing na fronteira streamed.
- [x] Adicionar testes ignorados com dependências npm reais.
- [x] Instalar e exercitar os quatro ZIPs no dev server.
- [x] Atualizar documentação, matriz, roadmap e evidência.

## Verification

```bash
cargo test -p edger-isolation --features multiproc --test uds_roundtrip node_http_proxy_preserves_real_request_and_response_semantics
cargo test -p edger-orchestrator --test framework_compat nestjs_express_and_fastify_cover_enterprise_http_features -- --ignored
cargo test -p edger-orchestrator --test framework_compat fastify_and_koa_cover_server_lifecycle_and_middleware -- --ignored
curl -H 'authorization: Bearer <root>' http://127.0.0.1:19080/metrics/stats
```

Prova live: Nest Express/Fastify tiveram sete requests cada; Fastify/Koa seis.
Todos terminaram `healthy`, `idle`, com um processo e `recycle.error = 0`.

## Status

**completed** (2026-07-17).
