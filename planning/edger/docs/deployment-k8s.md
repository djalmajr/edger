# Deployment: edger em Kubernetes (stateless + HPA)

**Status:** stub — preenchido pela Story 17.F (`epics/17-edger-minimalista/06-deployment-k8s.md`).

Padrão-alvo: Deployment **stateless** (sem PVC), Secret da root-key montado como
arquivo (`EDGER_ROOT_KEY_FILE`, rotação sem restart via hot-reload), envs
`EDGER_OIDC_*`, HPA por CPU/RPS, e o API Gateway externo na frente. Escala fina
(pool por worker + tuning de HPA) fica no Epic 18. Manifesto e operação nesta story.
