# Deployment: EdgeR em Kubernetes (stateless + HPA)

**Status:** chart de referência criado e validado em cluster real em 2026-07-03
pela Story 17.F (`planning/edger/epics/17-edger-minimalista/06-deployment-k8s.md`).

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

## Escala: L1 por worker e L2 por HPA

O chart expõe apenas escala L2 de réplicas do EdgeR. `charts/edger/values.yaml`
define `replicaCount` e `hpa.enabled`, `hpa.minReplicas`,
`hpa.maxReplicas`, `hpa.targetCPUUtilizationPercentage`; o
`charts/edger/questions.yaml` expõe esses campos no grupo `Scaling`; e
`charts/edger/templates/hpa.yaml` renderiza um
`HorizontalPodAutoscaler` `autoscaling/v2` apontando para a Deployment.

`maxProcesses`, `minProcesses`, `concurrency`, `queueLimit` e `queueTimeout`
são configuração L1 por worker no manifesto, não env global do pod. Por isso o
chart não cria um valor global de pool: um único knob de Deployment misturaria
workloads diferentes e esconderia o orçamento de memória por processo.

Use L1 para remover head-of-line blocking de um worker quente dentro da réplica;
use L2 para multiplicar a capacidade total com mais pods. HPA sozinho não
resolve o caso em que cada réplica mantém `maxProcesses: 1` e o mesmo worker
serializa requests dentro de cada pod. A operação completa está descrita em
`planning/edger/docs/scaling.md`.

## Validação em cluster (2026-07-03)

- Cluster: K3s v1.35.5 single-node (Hetzner , x86_64, Ubuntu 24.04); helm v4.2.2.
- Imagem: cross-compile local + montagem docker buildx amd64 (216MB) — nota: rustc SEGFAULTa sob emulação x86 no Docker Desktop/Apple Silicon; zigbuild é a rota para builds amd64 locais. Import direto no containerd (sem registry, validação apenas).
- `helm install` → sucesso; pod 1/1 Running em 11s, sem restart.
- Provas in-cluster (port-forward svc :3000): `/health` 200, `/ready` 200, `/` → 307 `/cpanel/`, cPanel servido do pod, admin sem key 401, admin com a chave do Secret-arquivo 200.
- Rotação de root key sem restart: chave nova aceita após ~75s (re-projeção do kubelet), chave antiga 401, restartCount=0.
- Limpeza completa: helm uninstall + namespace e imagem removidos.
- Pendência que resta: exposição via Cloudflare Tunnel/ingress e deploy contínuo (kaniko/registry, runbook 08 do infra) ficam para quando o edger for a produção de fato.
