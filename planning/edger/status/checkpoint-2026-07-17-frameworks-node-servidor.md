# Checkpoint 2026-07-17: frameworks Node de servidor

NestJS 11.1.6 foi validado com Express e Fastify; Fastify 5.6.1 e Koa 3.0.1
também passaram como servidores puros. A prova cobriu rotas, POST JSON,
validação, state warm, hooks/middleware, guard/interceptor e streaming.

Os quatro pacotes fonte em `workers/framework-tests/` foram enviados como ZIP
pela Admin API e instalados com HTTP 201. No runtime live, Nest Express/Fastify
acumularam sete requests cada e Fastify/Koa seis, todos `healthy`, `idle`, com um
processo persistente e `recycle.error = 0`.

O contrato novo `nodeHttpProxy: true` mantém um servidor Node real em socket
Unix privado. A fronteira streamed remove headers hop-by-hop e `Content-Length`
do servidor interno para que o outer HTTP server consuma o frame de término sem
reciclar falsamente o processo.

Evidência executável:

- `crates/edger-orchestrator/tests/framework_compat.rs`;
- `crates/edger-isolation/tests/uds_roundtrip.rs`;
- `planning/edger/docs/frameworks-deno-ssr.md`;
- `planning/edger/docs/compat-matrix.md`.
