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
| `charts/edger/Chart.yaml` | create | Metadados do chart |
| `charts/edger/questions.yaml` | create | Form de instalação Rancher-style (grupos Runtime/Scaling/Resources/Auth) |
| `charts/edger/values.yaml` | create | Defaults (image, replicas, OIDC, root-key, HPA, ingress do cPanel) |
| `charts/edger/templates/` | create | `deployment.yaml` (stateless), `service.yaml`, `ingress.yaml` (serve o cPanel), `hpa.yaml`, `secret.yaml` (root-key como arquivo) |
| `Dockerfile`, `.dockerignore` | create | Imagem multi-stage com binário `edger`, Deno no PATH e cPanel embarcado |
| `planning/edger/docs/deployment-k8s.md` | create | Operação: rotação sem restart, envs OIDC, API GW na frente, scaling (aponta Epic 18) |

## Detail
### Scope
- **In:** Helm chart estilo Rancher (`questions.yaml` + templates stateless) modelado no Buntime, adaptado; Ingress que serve o cPanel; doc de operação.
- **Out:** publicar o chart num repositório/marketplace; o API GW em si (doc 17.D dá o padrão); tuning fino de HPA (Epic 18); pipeline `generate-helm-*` do Buntime (nossas questions são poucas, escritas à mão).

### Acceptance criteria
- [x] Chart instalável (`helm install`/`helm template` válido) com `questions.yaml` Rancher-style: grupos **Runtime** (log level, pool size, worker dirs), **Scaling** (HPA on/off, min/max, target CPU), **Resources** (requests/limits, memory cap por worker), **Auth** (OIDC issuer/audience/roles-claim, root-key).
- [x] Templates: Deployment **stateless** (sem PVC), Secret da root-key montado como **arquivo** em `EDGER_ROOT_KEY_FILE`, envs `EDGER_OIDC_*`, HPA, e Ingress apontando para o Service que serve o cPanel.
- [x] Instalar preenchendo o form deixa o edger no ar **servindo a interface** (cPanel acessível pelo host do Ingress).
- [x] Doc mostra rotação de root-key **sem restart** (atualiza Secret → arquivo remontado → hot-reload da 17.A) e posiciona o API GW externo na frente.
- [x] Sem StatefulSet/PVC/turso no chart.

### Dependencies
- Stories 17.A–17.E

## Tasks
- [x] 2026-07-03 — Chart `charts/edger` (Chart/values/questions/templates) modelado no `../buntime/charts`, adaptado stateless.
- [x] 2026-07-03 — `questions.yaml` com grupos Runtime/Scaling/Resources/Auth (sem Persistence/Turso).
- [x] 2026-07-03 — Ingress servindo o cPanel por path configurável; validação estática via `helm lint`/`helm template` quando o binário `helm` estiver disponível.
- [x] 2026-07-03 — Dockerfile multi-stage com runtime Deno, binário `edger`, cPanel embarcado, usuário não-root e `.dockerignore`.
- [x] 2026-07-03 — Doc/story de operação registra rotação por Secret-arquivo, OIDC genérico opt-in, API Gateway externo e validação real de `helm install` em cluster real.

## Implementation Notes

- 2026-07-03 — O chart entregue em `charts/edger/` é stateless: usa Deployment, ConfigMap, Secret opcional, Service, Ingress opcional e HPA opcional; não cria PVC, StatefulSet, banco ou recursos Turso.
- 2026-07-03 — `rootKey.value` gera um Secret do chart e `rootKey.existingSecret` referencia um Secret externo. Em ambos os casos o pod recebe `EDGER_ROOT_KEY_FILE=/var/run/secrets/edger-root/root-key`; rotação real em cluster fica para o harness/usuário validar, porque esta execução não deve rodar `helm install`.
- 2026-07-03 — OIDC genérico está disponível e permanece opt-in no form (`oidc.enabled` + `EDGER_OIDC_*`), sem bloquear o deploy root-key.

## Validação em cluster (2026-07-03)

- Cluster: K3s v1.35.5 single-node (Hetzner , x86_64, Ubuntu 24.04); helm v4.2.2.
- Imagem: cross-compile local + montagem docker buildx amd64 (216MB) — nota: rustc SEGFAULTa sob emulação x86 no Docker Desktop/Apple Silicon; zigbuild é a rota para builds amd64 locais. Import direto no containerd (sem registry, validação apenas).
- `helm install` → sucesso; pod 1/1 Running em 11s, sem restart.
- Provas in-cluster (port-forward svc :3000): `/health` 200, `/ready` 200, `/` → 307 `/cpanel/`, cPanel servido do pod, admin sem key 401, admin com a chave do Secret-arquivo 200.
- Rotação de root key sem restart: chave nova aceita após ~75s (re-projeção do kubelet), chave antiga 401, restartCount=0.
- Limpeza completa: helm uninstall + namespace e imagem removidos.
- Pendência que resta: exposição via Cloudflare Tunnel/ingress e deploy contínuo (kaniko/registry, runbook 08 do infra) ficam para quando o edger for a produção de fato.

## Verification
```bash
# revisão do manifesto; rotação: kubectl apply do Secret e confirmar hot-reload sem restart
```
