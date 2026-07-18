# Epic 24: Frameworks Deno SSR

**Origin:** pedido do usuário em 2026-07-17 e contratos oficiais em
<https://docs.deno.com/deploy/reference/frameworks/>.

## Context

O runtime já executava Hono, SvelteKit e TanStack, mas não cobria toda a lista
oficial do Deno Deploy nem aceitava o tamanho de um Next standalone no fluxo
administrativo.

## Objective

Executar e implantar os nove caminhos da lista oficial: Next.js, Astro, Nuxt,
SolidStart, TanStack Start, SvelteKit, Fresh, Lume e Remix, mantendo a aplicação
montada em `/<worker>` e sem abrir uma porta TCP privada por app.

## Architecture boundaries

- Next.js standalone usa um servidor `node:http` real em socket Unix privado;
  o worker não recebe permissão de rede só para atender o proxy interno.
- Nuxt e SolidStart usam o preset Nitro `deno_deploy`; Astro usa o adapter Deno.
- Fresh preserva o entrypoint no caminho do build; Lume usa somente o `_site`
  estático; Remix/React Router usa o mesmo proxy Node privado do Next.
- Cada build declara `adapter`, `ssrEntrypoint`, `clientDir` quando aplicável e
  `basePath` compatível com o nome montado.
- O limite de request normal continua 4 MiB; apenas pacotes administrativos de
  deploy/upload/download usam 64 MiB, com expansão ZIP limitada a 256 MiB e
  50.000 entradas.
- Suporte significa o caminho exercitado de SSR, API e assets/hidratação; não é
  uma promessa irrestrita sobre toda API Node ou feature experimental.

## Story backlog

| Story | Arquivo | Objetivo | Status |
|---|---|---|---|
| 24.01 Adapters e deploy real | `01-adapters-deploy-real.md` | Runtime, manifests, pacote administrativo e evidência dos quatro frameworks | completed |
| 24.02 Lista oficial completa | `02-lista-oficial-completa.md` | Fresh, Lume, Remix e revalidação dos nove caminhos | completed |
| 24.03 Frameworks Node de servidor | `03-frameworks-node-servidor.md` | NestJS, Fastify, Koa e proxy Node declarativo | completed |

## Epic acceptance criteria

- [x] Next.js standalone responde HTML SSR e API por socket Unix privado.
- [x] Nuxt `deno_deploy` responde HTML SSR e API.
- [x] Astro Deno responde HTML SSR e API pelo entrypoint exportado/capturado.
- [x] SolidStart `deno_deploy` responde HTML SSR, API e asset hidratável.
- [x] ZIP Next real acima de 4 MiB instala com `201` sem elevar o limite do data plane.
- [x] Download integral do projeto Next retorna ZIP válido acima de 4 MiB.
- [x] Contratos, receitas e limites ficam documentados na matriz canônica.
- [x] TanStack Start e SvelteKit permanecem funcionais na rodada consolidada.
- [x] Fresh responde SSR, API e island hidratável sem perder seus assets.
- [x] Lume serve raiz, rota de diretório e asset do `_site` sem processo JS.
- [x] Remix/React Router responde SSR, API e hidratação pelo socket Unix privado.
- [x] NestJS funciona com Express e Fastify, preservando DI/state warm.
- [x] Fastify e Koa funcionam como servidores persistentes sem porta TCP do app.
- [x] Framing externo não recicla respostas Node finitas com `Content-Length`.

## Status

completed (2026-07-17).
