# Story 16.C: Spike — TanStack Start rodando no EdgeR (best-effort)

**Origin:** `planning/edger/epics/16-fullstack-ssr-streaming/00-overview.md`

## Context

- **Problema:** TanStack Start é de interesse do operador, mas é novo e mudou de fundação recentemente (Vinxi → Vite) — churn alto. Ninguém provou o build rodando nas capturas.
- **Objetivo:** buildar um app TanStack Start real com preset node/deno e rodá-lo como `kind: fetch`; registrar findings honestos, **mesmo se falhar**.
- **Valor:** evidência para decidir se vira caminho suportado agora ou espera a v1 estabilizar.
- **Restrições:** best-effort explícito — falha documentada é resultado válido do spike.

## Traceability

- `edger-isolation/src/multiproc_harness.mjs` (capturas)
- `planning/edger/docs/compat-matrix.md`

## Files

| Path | Action | Reason |
|---|---|---|
| scratchpad (fora do repo) | create | Projeto TanStack Start + build |
| `workers/tanstack-demo/` | create (condicional) | Fixture, apenas se passar e for enxuto |
| `planning/edger/docs/compat-matrix.md` | edit | Linha TanStack Start com finding |
| `planning/edger/epics/16-fullstack-ssr-streaming/03-spike-tanstack-start.md` | edit | Findings no Status |

## Detail

### Scope
- **In:** app mínimo (rota SSR + server function/API), build com target node ou deno, rodar no EdgeR live, findings.
- **Out:** compromisso de suporte; investigação profunda de bugs do framework.

### Acceptance criteria
- [x] Build real executado contra o EdgeR: SSR, server route, assets, hidratação e navegação client-side funcionando ao vivo (screenshot + snapshot no preview).
- [x] compat-matrix atualizada: TanStack Start `tested`, com a receita de build documentada.

### Dependencies
- Story 16.A

## Tasks
- [x] App via `@tanstack/cli create` (react, non-interactive) + rota server `/api/info`; build Vite.
- [x] Iterações da receita: `ssr.noExternal` (deps transitivas), `base`/`basepath` (mount), wrapper de estáticos + restauração de base.
- [x] Findings no Status + compat-matrix; fixture `workers/tanstack-demo`.

## Verification

```bash
curl -H "Authorization: Bearer $KEY" http://127.0.0.1:3000/tanstack-demo
```

## Revalidação 2026-07-03

O wrapper `workers/tanstack-demo/index.mjs` foi endurecido para servir estáticos
apenas de `/assets/*` e dos arquivos públicos presentes no build
(`favicon.ico`, `logo192.png`, `logo512.png`, `manifest.json`, `robots.txt`).
Paths malformados retornam 400; traversal/absolutos fora do contrato retornam
404. A validação live pós-Epic 17 deve ser executada pelo harness fora do
sandbox e anexada pelo coordenador.

## Status

**completed** (2026-07-02) — TanStack Start roda no EdgeR: fixture `workers/tanstack-demo`
(build Vite real via `@tanstack/cli create`) serve SSR (`/`, `/about`), server route
`/api/info`, assets estáticos, hidratação React e navegação client-side — validado ao
vivo no preview (screenshot + snapshot; zero erros de console; warm ~5ms). O contrato do
build é diferente do SvelteKit: `dist/server/server.js` exporta um **fetch handler puro**
(`createStartHandler` — objeto `{ fetch }`), sem servidor próprio e sem servir assets.
Três ajustes de receita foram necessários (todos de build/config, zero mudança no runtime):

1. **`ssr: { noExternal: true }`** no vite.config — o bundle default importa deps
   transitivas (`@tanstack/router-core` etc.) que o Deno recusa (`not a dependency`:
   sem hoisting de node_modules); bundlado fica self-contained (~870K), só node builtins.
2. **`base: /tanstack-demo/` + `router.basepath`** no build — o HTML SSR referencia
   assets em paths absolutos; sem base, escapam do mount do worker.
3. **Wrapper `index.mjs`** (~30 linhas): serve `./client/*` estático e restaura a base
   (via header `x-base`) antes de delegar — o router espera path completo, o EdgeR
   entrega relativo (filosofia Buntime). O wrapper é exatamente o que um `adapter:
   tanstack` declarativo automatizaria no futuro.

Ressalva de churn mantida: TanStack Start ainda é novo (a receita pode mudar entre
versões); status `tested` vale para o snapshot atual (@tanstack/react-start via plugin
Vite, 2026-07).
