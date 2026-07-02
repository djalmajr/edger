# Story 16.B: Spike — SvelteKit rodando no EdgeR

**Origin:** `planning/edger/epics/16-fullstack-ssr-streaming/00-overview.md`

## Context

- **Problema:** SvelteKit é o candidato prioritário de framework "de build" (leve, bem-fatorado), mas ninguém provou que o output do build roda pelas capturas do processo persistente.
- **Objetivo:** construir um app SvelteKit real, buildar com adapter (deno e/ou node) e rodá-lo como `kind: fetch` no EdgeR; registrar findings honestos (funciona / o que falta / o que quebra).
- **Valor:** decide com evidência se SvelteKit entra como caminho suportado ou o que falta para isso.
- **Restrições:** spike — o critério é o *finding*, não shipping de feature. Fixture só entra no repo se o resultado for reprodutível e enxuto.

## Traceability

- `edger-isolation/src/multiproc_harness.mjs` (capturas `Deno.serve` e `node:http`)
- `planning/edger/docs/compat-matrix.md`

## Files

| Path | Action | Reason |
|---|---|---|
| scratchpad (fora do repo) | create | Projeto SvelteKit + builds (adapter-deno/adapter-node) |
| `workers/sveltekit-demo/` | create (condicional) | Build output como fixture, se passar e for enxuto |
| `planning/edger/docs/compat-matrix.md` | edit | Linha SvelteKit com finding |
| `planning/edger/epics/16-fullstack-ssr-streaming/02-spike-sveltekit.md` | edit | Registrar findings no Status |

## Detail

### Scope
- **In:** criar app mínimo (página SSR + rota `+server.ts`), build com `svelte-adapter-deno` (preferência: cai na captura `Deno.serve`) e fallback `adapter-node` (captura `node:http`), rodar no EdgeR live, findings.
- **Out:** compromisso de suporte; adapter declarativo; assets pelo caminho Rust.

### Acceptance criteria
- [x] Build real de SvelteKit executado contra o EdgeR (adapter-node 5.x, app `sv create` minimal + página SSR `load` + rota `+server.ts`).
- [x] Finding registrado: SSR, API e assets estáticos respondem 200 pelo processo persistente (warm ~2.7ms); dois gaps do harness encontrados e corrigidos (ver Status).
- [x] compat-matrix atualizada: SvelteKit `tested`.

### Dependencies
- Story 16.A

## Tasks
- [x] App mínimo (`sv create` minimal/ts) + build `@sveltejs/adapter-node` (config no `vite.config.ts` nas versões novas); sanity sob Deno puro (SSR+API+assets OK); rodar no EdgeR.
- [x] Findings no Status + compat-matrix; fixture `workers/sveltekit-demo` (build self-contained, sem node_modules).

## Verification

```bash
curl -H "Authorization: Bearer $KEY" http://127.0.0.1:3000/sveltekit-demo
```

## Status

**completed** (2026-07-02) — SvelteKit roda no EdgeR: fixture `workers/sveltekit-demo`
(build real `@sveltejs/adapter-node`, self-contained, 1.6M) serve página SSR (`load`
executado no servidor), rota de API `+server.ts` e assets `_app/immutable/*` — tudo 200
ao vivo no preview, warm ~2.7ms, hidratação sem erros de console. O spike encontrou e
corrigiu **dois gaps reais do harness** (ambos capturados por mutação no E2E
`polka_style_on_request_capture_and_host_header`):

1. **Captura de um único listener `request`:** o adapter-node cria `createServer()`
   sem argumento e registra o handler do polka **e** um listener de tracking de
   shutdown via `server.on("request", ...)`. O harness guardava só o último — o
   dispatch ia para o tracker, que nunca responde, e o await pendente deixava o event
   loop drenar: o processo saía **limpo (exit 0)** no meio do request. Agora captura
   TODOS os listeners e invoca todos (semântica Node real).
2. **Host header ausente:** o construtor `Request` descarta `Host` (forbidden header)
   e o `getRequest` do SvelteKit responde 400 sem ele. O adapter node do harness agora
   defaulta `headers.host` a partir da URL.

Sanity prévia: o mesmo build roda 100% sob Deno puro (`deno run -A build/index.js`) —
num container, o servidor do próprio framework dispensa adapter; a captura existe
porque no EdgeR o ingress é do orquestrador. Nota de versão: nos templates novos do
`sv create`, a config do adapter fica no `vite.config.ts` (plugin `sveltekit()`), não
em `svelte.config.js`.
