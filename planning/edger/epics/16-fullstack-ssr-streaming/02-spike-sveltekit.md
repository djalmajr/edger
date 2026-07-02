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
- [ ] Build real de SvelteKit executado contra o EdgeR (não simulação).
- [ ] Finding registrado: rota SSR e rota de API respondem? Assets estáticos servem? O que falhou?
- [ ] compat-matrix atualizada com status honesto (`tested`/`partial`/`gap`).

### Dependencies
- Story 16.A

## Tasks
- [ ] App mínimo + build adapter-deno; rodar no EdgeR; se falhar, adapter-node.
- [ ] Findings no Status + compat-matrix; fixture se couber.

## Verification

```bash
curl -H "Authorization: Bearer $KEY" http://127.0.0.1:3000/sveltekit-demo
```
