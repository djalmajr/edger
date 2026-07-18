# Frameworks Deno SSR no EdgeR

O EdgeR cobre os nove frameworks da lista oficial do Deno Deploy: Next.js,
Astro, Nuxt, SolidStart, TanStack Start, SvelteKit, Fresh, Lume e Remix. As
receitas partem dos caminhos oficiais, mas o build também precisa conhecer o
subpath `/<worker>` usado pelo EdgeR. Remix continua marcado como experimental
na documentação do Deno; Lume é entregue como site estático pré-gerado.

## Contrato comum

```yaml
name: meu-app
version: "1.0.0"
kind: fullstack
adapter: nextjs
ssrEntrypoint: caminho/do/entrypoint
basePath: /meu-app
timeout: 60s
memoryMb: 512
```

O `basePath` e a configuração do framework devem representar o mesmo caminho.
Adapters aceitos: `nextjs`, `astro`, `nuxt`, `solidstart`, `tanstack`,
`sveltekit`, `fresh`, `lume` e `remix`. Aliases como `next`, `nuxtjs`, `solid`,
`tanstack-start`, `svelte` e `react-router` são normalizados para os nomes
canônicos.

## Next.js

Use o output standalone oficial:

```js
// next.config.mjs
export default {
  basePath: "/meu-app",
  output: "standalone",
};
```

Após `next build`, empacote o conteúdo de `.next/standalone` como raiz e copie,
quando existirem, `public` e `.next/static` para as mesmas posições dentro do
standalone. Manifesto:

```yaml
adapter: nextjs
ssrEntrypoint: server.js
basePath: /meu-app
memoryMb: 1024
```

O EdgeR intercepta `http.createServer().listen()` e liga o servidor real a um
socket Unix privado. Assim o Next recebe `IncomingMessage` e `ServerResponse`
genuínos sem porta TCP e sem `allow-net` adicional.

## Nuxt

Configure a base e gere o preset oficial Nitro:

```ts
// nuxt.config.ts
export default defineNuxtConfig({
  app: { baseURL: "/meu-app/" },
});
```

```sh
nuxi build --preset=deno_deploy
```

Empacote `.output` e use:

```yaml
adapter: nuxt
ssrEntrypoint: .output/server/index.ts
clientDir: .output/public
basePath: /meu-app
```

## Astro

Use o adapter Deno e a mesma base:

```js
// astro.config.mjs
import deno from "@deno/astro-adapter";
import { defineConfig } from "astro/config";

export default defineConfig({
  adapter: deno(),
  base: "/meu-app",
  output: "server",
});
```

O entrypoint oficial é `dist/server/entry.mjs`. Para um ZIP autocontido, gere
o bundle no mesmo diretório, preservando a relação com `dist/client`:

```sh
deno bundle dist/server/entry.mjs --output dist/server/entry.bundle.mjs
```

```yaml
adapter: astro
ssrEntrypoint: dist/server/entry.bundle.mjs
clientDir: dist/client
basePath: /meu-app
```

Também é possível enviar a árvore de dependências resolvível pelo Deno em vez
do bundle. O runtime reconhece o `handle` exportado pelo adapter e continua
capturando `Deno.serve` para entrypoints convencionais.

## SolidStart

Use o preset oficial e configure as bases dos routers Vinxi, pois o template
v1 assume deployment na raiz:

```ts
// app.config.ts
import { defineConfig } from "@solidjs/start/config";

const base = "/meu-app";
const app = defineConfig({ server: { preset: "deno_deploy" } });

for (const router of app.config.routers) {
  if (router.name === "client") router.base = `${base}/_build`;
  else if (router.name === "server-fns") router.base = `${base}/_server`;
  else router.base = base;
}

export default app;
```

O `Router` da aplicação também recebe `base="/meu-app"`. Rotas API do
filesystem precisam incluir esse segmento enquanto o SolidStart v1 não remove
a base antes do matcher (por exemplo,
`src/routes/meu-app/api/info.ts`). Depois de `vinxi build`:

```yaml
adapter: solidstart
ssrEntrypoint: .output/server/index.ts
clientDir: .output/public/meu-app
basePath: /meu-app
```

## TanStack Start e SvelteKit

Os dois caminhos já têm fixtures de referência no repositório:

- `workers/examples/tanstack-demo`: build Vite, `adapter: tanstack`,
  `ssrEntrypoint: server/server.js` e `clientDir: client`;
- `workers/examples/sveltekit-demo`: `@sveltejs/adapter-node`, paths absolutos
  com a base do worker e wrapper que restaura a base antes do handler.

Ambos foram revalidados com SSR, endpoint server/API, assets e hidratação.

## Fresh

Configure `basePath` no Fresh/Vite e gere o build de produção. O entrypoint
deve permanecer na árvore `_fresh/server`, porque o cache de build resolve os
assets em relação a `import.meta.dirname`:

```yaml
adapter: fresh
ssrEntrypoint: _fresh/server/server-entry.mjs
basePath: /meu-app
memoryMb: 256
```

O EdgeR executa esse entrypoint no local original, sem rebundle relocável, mas
mantém a leitura limitada ao diretório do worker. A validação usou Fresh 2,
página SSR, island Preact hidratável, API e assets `_fresh/client`.

## Lume

Rode o build do Lume e empacote `_site`. O adapter usa o servidor estático Rust
e reconhece `index.html`, diretórios com `index.html` e rotas HTML limpas:

```yaml
adapter: lume
ssrEntrypoint: _site/index.html
clientDir: _site
basePath: /meu-app
```

O `<base>` é ajustado para o subpath do worker. Lume não cria um processo Deno
por request: o resultado validado é o site estático pré-gerado.

## Remix / React Router

O caminho atual usa React Router framework mode, sucessor do Remix, com
`basename`/`base` iguais a `/meu-app`. Gere o server build com entry server Web
Streams (`renderToReadableStream`) e empacote um servidor Node autocontido:

```yaml
adapter: remix
ssrEntrypoint: server.bundle.mjs
clientDir: build/client
basePath: /meu-app
memoryMb: 512
```

Assim como Next standalone, o servidor Express/Node roda em socket Unix
privado. Este adapter deve ser tratado como experimental enquanto o próprio
Deno Deploy mantiver essa classificação.

## Limites do pacote

Requests normais continuam limitados por `MAX_BODY_BYTES` (4 MiB). Instalação,
upload de arquivos e download integral de projeto usam o limite administrativo
de 64 MiB. A extração rejeita mais de 50.000 entradas ou mais de 256 MiB
descompactados, reduzindo risco de ZIP bomb.

## Escopo validado

A validação de 2026-07-17 cobriu os nove caminhos oficiais. Os oito frameworks
com runtime interativo responderam HTML SSR, endpoint server/API e assets do
cliente; Lume respondeu páginas e assets pré-gerados. Hidratação foi exercitada
nos frameworks interativos aplicáveis. O Next standalone de 11 MiB foi
instalado e baixado novamente como ZIP válido. ISR, PPR, image optimization,
websockets e outras combinações avançadas só devem ser marcadas como suportadas
após uma prova específica.

## Frameworks de servidor Node

Frameworks que dependem de `IncomingMessage`/`ServerResponse` completos podem
declarar o proxy HTTP privado no manifesto:

```yaml
kind: fetch
entrypoint: index.ts
nodeHttpProxy: true
```

O processo abre o servidor Node somente em um socket Unix temporário. O app não
recebe uma porta TCP pública e o tráfego continua passando pelo router, limites,
observabilidade e pool do EdgeR. Headers hop-by-hop e `Content-Length` do servidor
interno não são encaminhados: o servidor HTTP externo controla o framing, consome
o frame de término e mantém o processo reutilizável.

A rodada de 2026-07-17 validou:

- NestJS 11.1.6 com `ExpressAdapter` e `FastifyAdapter`: decorators,
  `reflect-metadata`, DI com estado warm, guard, interceptor, `ValidationPipe`,
  rota parametrizada, POST JSON e `StreamableFile`;
- Fastify 5.6.1: hooks, schema JSON, rota parametrizada, POST e stream Node;
- Koa 3.0.1: middleware em cascata, `AsyncLocalStorage`, router, body parser e
  stream Node.

Os quatro pacotes foram instalados por ZIP pela Admin API com HTTP 201. Após a
prova live, cada app permaneceu `healthy` e `idle`, com um processo persistente e
zero reciclagens por erro. WebSockets, Nest microservices e adapters de banco
continuam fora desta prova HTTP.

## Referências oficiais

- <https://docs.deno.com/deploy/reference/frameworks/>
- <https://fresh.deno.dev/docs/>
- <https://lume.land/docs/overview/installation/>
- <https://reactrouter.com/start/framework/installation>
- <https://nextjs.org/docs/app/api-reference/config/next-config-js/output>
- <https://nuxt.com/deploy/deno-deploy>
- <https://docs.solidjs.com/solid-start/reference/config/define-config>
