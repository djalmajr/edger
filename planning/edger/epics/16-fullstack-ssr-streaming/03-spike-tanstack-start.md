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
- [ ] Build real executado contra o EdgeR (ou falha de build/runtime documentada com causa).
- [ ] compat-matrix atualizada com status honesto (`tested`/`partial`/`gap`).

### Dependencies
- Story 16.A

## Tasks
- [ ] App mínimo + build preset node/deno; rodar no EdgeR.
- [ ] Findings no Status + compat-matrix.

## Verification

```bash
curl -H "Authorization: Bearer $KEY" http://127.0.0.1:3000/tanstack-demo
```
