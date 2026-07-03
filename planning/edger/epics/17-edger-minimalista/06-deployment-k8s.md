# Story 17.F: Deployment K8s de referência (stateless + Secret-arquivo)

**Origin:** `planning/edger/epics/17-edger-minimalista/00-overview.md`

## Context

- **Problema:** falta um manifesto de referência que reflita o edger stateless (sem PVC de auth, HPA-ready) com a auth OIDC/root-key da story 17.A e o API Gateway externo da 17.D.
- **Objetivo:** um manifesto K8s de referência + doc de operação: Deployment stateless do runtime, Secret montado como **arquivo** (root-key com rotação sem restart), envs OIDC, e o API GW externo na frente.
- **Valor:** operador tem o caminho pronto para rodar em cluster com auto-scale.
- **Restrições:** nada de StatefulSet/PVC para o edger (é stateless); estado é externo (worker escolhe).

## Traceability
- `planning/edger/docs/deployment-api-gateway.md` (17.D); envs `EDGER_OIDC_*`/`EDGER_ROOT_KEY_FILE` (17.A)

## Files
| Path | Action | Reason |
|---|---|---|
| `deploy/k8s/edger.yaml` | create | Deployment stateless + Service + Secret (root-key como arquivo) + HPA |
| `planning/edger/docs/deployment-k8s.md` | create | Operação: rotação de chave sem restart, envs, API GW na frente, notas de scaling (aponta Epic 18) |

## Detail
### Scope
- **In:** manifesto Deployment/Service/Secret/HPA de referência; doc de operação (rotação, OIDC, API GW).
- **Out:** Helm chart/Kustomize completos; o API GW em si (doc 17.D dá o padrão); tuning fino de HPA (Epic 18).

### Acceptance criteria
- [ ] Manifesto: Deployment **stateless** (sem PVC), Secret da root-key montado como arquivo em `EDGER_ROOT_KEY_FILE`, envs `EDGER_OIDC_*`, HPA por CPU/RPS.
- [ ] Doc mostra rotação de root-key **sem restart** (atualiza Secret → arquivo remontado → hot-reload da 17.A).
- [ ] Doc posiciona o API GW externo (OIDC/rate limit) na frente do edger.

### Dependencies
- Stories 17.A–17.E

## Tasks
- [ ] Manifesto de referência (Deployment/Service/Secret/HPA).
- [ ] Doc de operação (rotação sem restart, OIDC, API GW, ponteiro para Epic 18 de scaling).

## Verification
```bash
# revisão do manifesto; rotação: kubectl apply do Secret e confirmar hot-reload sem restart
```
