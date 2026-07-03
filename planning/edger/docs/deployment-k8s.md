# Deployment: EdgeR em Kubernetes (stateless + HPA)

**Status:** chart de referência criado em 2026-07-03 pela Story 17.F
(`planning/edger/epics/17-edger-minimalista/06-deployment-k8s.md`).

O chart vive em `charts/edger/` e segue o formato Rancher Apps & Marketplace com
`questions.yaml`. O deploy é **stateless**: Deployment, Service, ConfigMap,
Secret opcional, Ingress opcional e HPA opcional; sem StatefulSet, PVC, banco ou
Turso.

## Auth

- `rootKey.value`: gera um Secret do chart.
- `rootKey.existingSecret`: referencia um Secret já existente.
- Ambos montam a chave em `/var/run/secrets/edger-root/root-key` e configuram
  `EDGER_ROOT_KEY_FILE` para esse caminho. A rotação acontece atualizando o
  Secret; o runtime relê o arquivo sem restart.
- OIDC é fase 2 e fica opcional via `oidc.enabled`, `oidc.issuer`,
  `oidc.audience`, `oidc.rolesClaim` e `oidc.requiredRole`, renderizados como
  `EDGER_OIDC_*`.

## Operação

- `/livez` é a probe de liveness.
- `/ready` é a probe de readiness real.
- `/` redireciona para `/cpanel/`; o Ingress usa `ingress.path` configurável,
  com default `/`.
- O API Gateway externo fica na frente do Service/Ingress quando necessário; o
  chart não embute gateway stateful nem armazenamento.
- Escala fina de pool por worker e tuning adicional de HPA permanecem no Epic 18.

Validação real com `helm install` em cluster fica com o harness/usuário. Nesta
story a validação esperada é estática: `helm lint`, `helm template` ou parser
YAML quando `helm` não estiver disponível.
