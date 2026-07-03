# Story 17.F: Deployment K8s de referência (stateless + Secret-arquivo)

**Origin:** `planning/edger/epics/17-edger-minimalista/00-overview.md`

## Context

- **Problema:** falta um chart de referência que reflita o edger stateless (sem PVC de auth, HPA-ready) com a auth OIDC/root-key da story 17.A e o API Gateway externo da 17.D. Além disso, o operador quer instalação **form-driven** (estilo Rancher Apps & Marketplace) que já sirva a interface (cPanel) ao final.
- **Objetivo:** um **Helm chart estilo Rancher com `questions.yaml`** (form de instalação) modelado no que o Buntime já tem (`../buntime/charts`), **adaptado para o edger stateless**: Deployment (não StatefulSet), Secret montado como **arquivo** (root-key, rotação sem restart), envs OIDC, HPA, e Ingress que serve a interface (cPanel). Instalar pelo form provisiona e serve o cPanel.
- **Valor:** operador instala via UI (Rancher/marketplace) preenchendo um form, e no fim tem o edger no ar servindo a interface — sem editar YAML na mão.
- **Restrições:** nada de StatefulSet/PVC/turso-server no chart do edger (é stateless; o Buntime tem esses porque é stateful). Estado é externo (worker escolhe). Reusar a **estrutura** das questions do Buntime (grupos/tipos/labels), não copiar os campos de Turso/Persistence.

### Referência: chart do Buntime
`../buntime/charts` — Helm chart Rancher-style: `questions.base.yaml` → `questions.yml` (auto-gerado por `scripts/generate-helm-questions.ts`), `values.yaml`, `templates/` (deployment/statefulset, service, ingress, hpa, secret, configmap, pvc, turso-server...). Grupos de questions: Runtime, Scaling, Resources, Persistence, Turso Server, Turso Backup.
**Para o edger:** manter grupos **Runtime, Scaling, Resources, Auth**; descartar **Persistence/Turso** (stateless).

## Traceability
- `planning/edger/docs/deployment-api-gateway.md` (17.D); envs `EDGER_OIDC_*`/`EDGER_ROOT_KEY_FILE` (17.A)

## Files
| Path | Action | Reason |
|---|---|---|
| `deploy/helm/edger/Chart.yaml` | create | Metadados do chart |
| `deploy/helm/edger/questions.yaml` | create | Form de instalação Rancher-style (grupos Runtime/Scaling/Resources/Auth) |
| `deploy/helm/edger/values.yaml` | create | Defaults (image, replicas, OIDC, root-key, HPA, ingress do cPanel) |
| `deploy/helm/edger/templates/` | create | `deployment.yaml` (stateless), `service.yaml`, `ingress.yaml` (serve o cPanel), `hpa.yaml`, `secret.yaml` (root-key como arquivo) |
| `planning/edger/docs/deployment-k8s.md` | create | Operação: rotação sem restart, envs OIDC, API GW na frente, scaling (aponta Epic 18) |

## Detail
### Scope
- **In:** Helm chart estilo Rancher (`questions.yaml` + templates stateless) modelado no Buntime, adaptado; Ingress que serve o cPanel; doc de operação.
- **Out:** publicar o chart num repositório/marketplace; o API GW em si (doc 17.D dá o padrão); tuning fino de HPA (Epic 18); pipeline `generate-helm-*` do Buntime (nossas questions são poucas, escritas à mão).

### Acceptance criteria
- [ ] Chart instalável (`helm install`/`helm template` válido) com `questions.yaml` Rancher-style: grupos **Runtime** (log level, pool size, worker dirs), **Scaling** (HPA on/off, min/max, target CPU), **Resources** (requests/limits, memory cap por worker), **Auth** (OIDC issuer/audience/roles-claim, root-key).
- [ ] Templates: Deployment **stateless** (sem PVC), Secret da root-key montado como **arquivo** em `EDGER_ROOT_KEY_FILE`, envs `EDGER_OIDC_*`, HPA, e Ingress apontando para o Service que serve o cPanel.
- [ ] Instalar preenchendo o form deixa o edger no ar **servindo a interface** (cPanel acessível pelo host do Ingress).
- [ ] Doc mostra rotação de root-key **sem restart** (atualiza Secret → arquivo remontado → hot-reload da 17.A) e posiciona o API GW externo na frente.
- [ ] Sem StatefulSet/PVC/turso no chart.

### Dependencies
- Stories 17.A–17.E

## Tasks
- [ ] Chart `deploy/helm/edger` (Chart/values/questions/templates) modelado no `../buntime/charts`, adaptado stateless.
- [ ] `questions.yaml` com grupos Runtime/Scaling/Resources/Auth (sem Persistence/Turso).
- [ ] Ingress servindo o cPanel; validar `helm template` + (se disponível) `helm lint`.
- [ ] Doc de operação (rotação sem restart, OIDC, API GW, ponteiro para Epic 18).

## Verification
```bash
# revisão do manifesto; rotação: kubectl apply do Secret e confirmar hot-reload sem restart
```
