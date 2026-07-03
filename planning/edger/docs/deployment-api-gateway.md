# Deployment: API Gateway externo na frente do edger

**Status:** padrão atual desde a Story 17.D (`epics/17-edger-minimalista/04-remover-gateway.md`).

Padrão-alvo: um API Gateway externo (Kong/APISIX/Envoy/cloud LB) assume a borda —
auth OIDC, rate limit, cache, redirects, host routing — e encaminha ao edger, que
fica sendo runtime puro de workers. O edger mantém só o roteamento core
(nome/versão/semver → worker). Detalhamento e receita concreta nesta story.
