# Story 16.A: Hono SSR + JSX como caminho fullstack first-class

**Origin:** `planning/edger/epics/16-fullstack-ssr-streaming/00-overview.md`

## Context

- **Problema:** não existe caminho fullstack suportado; `kind: fullstack` responde 501. Mas o runtime atual já roda Hono, e Deno transpila TSX nativamente — SSR com JSX funciona sem build step (provado em scratchpad 2026-07-02).
- **Objetivo:** worker Hono SSR+JSX (`index.tsx` + `deno.json` com `jsxImportSource: hono/jsx`, middleware `jsxRenderer`) rodando pelo processo persistente como `kind: fetch`, com fixture, E2E, validação live e documentação como caminho recomendado.
- **Valor:** mata o gap fullstack com custo mínimo e DX superior (deploy de fonte, zero build) — alinhado à visão mini-Vercel.
- **Restrições:** zero código novo de runtime (usa a captura `Deno.serve` existente); jsxRenderer rende string finita (casa com o contrato buffered atual).

## Traceability

- hono.dev/docs/guides/jsx + hono.dev/docs/middleware/builtin/jsx-renderer (referências do operador)
- `crates/edger-isolation/src/multiproc_harness.mjs` (captura `Deno.serve`)
- `planning/edger/docs/compat-matrix.md`

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/examples/ssr-demo/index.tsx` | create | Fixture SSR: layout via `jsxRenderer`, página renderada no servidor, rota de API JSON |
| `workers/examples/ssr-demo/deno.json` | create | `compilerOptions.jsx: precompile`, `jsxImportSource: hono/jsx`, import map hono |
| `workers/examples/ssr-demo/manifest.yaml` | create | `entrypoint: index.tsx`, `kind: fetch` |
| `crates/edger-orchestrator/tests/framework_compat.rs` | edit | E2E: SSR HTML + rota API via processo persistente (ignored, precisa deno+npm) |
| `planning/edger/docs/compat-matrix.md` | edit | Linha "Hono SSR + JSX (fullstack blessed path)" |

## Detail

### TO-BE
- `workers/examples/ssr-demo`: app Hono com `jsxRenderer` (layout HTML), página `/` renderada no servidor com dados dinâmicos, e rota `/api/info` JSON — o par SSR+API que caracteriza fullstack.
- Deno resolve o TSX no import do harness (o `--config deno.json` já é passado no spawn).

### Scope
- **In:** fixture, E2E, validação live no preview, compat-matrix/docs.
- **Out:** interatividade client-side (`hono/jsx/dom`)/islands; HonoX; template no botão Deploy do cPanel (follow-up de UX).

### Acceptance criteria
- [x] `GET /ssr-demo` retorna HTML renderado no servidor (200, `text/html`, conteúdo dinâmico do JSX) via processo persistente — validado ao vivo (warm ~1.8ms).
- [x] `GET /ssr-demo/api/info` retorna JSON — SSR e API no mesmo worker.
- [x] Fonte `.tsx` deployada diretamente, sem build step (Deno transpila via `--config deno.json` já passado no spawn).
- [x] compat-matrix documenta o caminho como blessed path fullstack.

### Dependencies
- Story 15.C (captura `Deno.serve` + npm no processo persistente)

## Tasks
### Fase 1 — Fixture
- [x] `workers/examples/ssr-demo` (index.tsx + deno.json + manifest); `ssr-demo` adicionado ao `shellExcludes` do shell-demo (roots de segmento único são interceptados pelo shell gateway — convenção dos demos).
### Fase 2 — Prova
- [x] E2E `hono_ssr_jsx_renders_html_on_the_server` (layout jsxRenderer + expressão dinâmica + API JSON); validado live no preview (página + screenshot).
### Fase 3 — Doc
- [x] compat-matrix: linha "Fullstack blessed path: Hono SSR + JSX" (tested).

## Verification

```bash
cargo test -p edger-orchestrator --test framework_compat -- --ignored
curl -H "Authorization: Bearer $KEY" http://127.0.0.1:3000/ssr-demo
```

## Status

**completed** (2026-07-02) — Caminho fullstack blessed entregue com zero código novo
de runtime: `workers/examples/ssr-demo` deploya um `index.tsx` como FONTE (sem build) — o Deno
transpila o JSX nativamente via `deno.json` (`jsx: precompile`, `jsxImportSource:
hono/jsx`), que o spawn do processo persistente já passa por `--config`. O worker usa
o middleware `jsxRenderer` (layout HTML) + página SSR com dados dinâmicos + rota de API
JSON no mesmo worker. Provado por E2E (`hono_ssr_jsx_renders_html_on_the_server`,
mutação: remover o pass-through do `--config` quebra o transform) e validado ao vivo
no preview builtin (HTML renderado no servidor, warm ~1.8ms; screenshot no browser).
Detalhe operacional descoberto: roots de segmento único (`/ssr-demo`) são interceptados
pelo shell gateway (story 07.02) — `ssr-demo` entrou no `shellExcludes` do `shell-demo`,
como os demais demos. Interatividade client-side (`hono/jsx/dom`)/HonoX e template no
botão Deploy do cPanel ficam como follow-ups.
