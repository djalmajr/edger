# Epic 11: Gateway Operacional Avancado

**Origin:** `planning/edger/roadmap.md`

**Depends on epic:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context

### Macro problem

O Epic 08 entregou gateway local com CORS, redirect, rate limit em memoria, diagnostics e Admin API read-only. O valor Buntime vai alem: proxy externo, cache, vhosts, historico operacional, streaming de eventos e mutacoes dinamicas. Esses itens precisam de um epic proprio porque misturam rede, estado duravel, seguranca e operacao.

### Initiative objective

Transformar o gateway em modulo operacional avancado, mantendo o orchestrator enxuto e `edger-core` puro. A implementacao deve ser localmente testavel, com provider duravel opcional onde fizer sentido, e sem deploy remoto nesta fase.

### AS-IS

- Redirect por prefixo, CORS/preflight e rate limit local ja existem.
- Gateway diagnostics, logs filtraveis, stats e duration media existem.
- `GatewayExtension::with_history_store` prova persistencia de decisoes via provider externo.
- Proxy externo, cache, vhosts, SSE e mutacoes dinamicas ainda nao foram fechados.

### TO-BE

- Proxy externo com allowlist, limites, timeout e protecoes contra SSRF.
- Cache e rate limit persistentes quando provider duravel estiver configurado.
- Historico/SSE local para operacao sem expor segredos.
- Host routing/vhosts como contrato seguro e testavel.

### Out of scope

- Deploy remoto, DNS gerenciado ou certificados publicos.
- WAF completo.
- Gateway distribuido multi-regiao.
- UI final de gateway; isso pertence ao Epic 12.

## Story backlog

| Story | Arquivo | Objetivo | Tamanho | Status | Depende de |
|---|---|---|---|---|---|
| 11.01 Proxy forwarding local | `01-proxy-forwarding-local.md` | Encaminhar requests para upstreams permitidos com limites e SSRF guard | large | planned | Epic 08.15, Epic 08.18 |
| 11.02 Cache e rate limit persistente | `02-cache-rate-limit-persistente.md` | Persistir cache/rate limit quando provider duravel estiver configurado | large | planned | 11.01, Epic 09 |
| 11.03 Historico e SSE operacional | `03-historico-sse-operacional.md` | Expor historico operacional e stream local de eventos seguros | medium | planned | 11.01, Epic 09 |
| 11.04 Vhosts e host routing | `04-vhosts-host-routing.md` | Resolver apps por host sem route hijack e com evidencia local | medium | planned | 11.01 |

## Epic acceptance criteria

- [ ] Proxy externo valida allowlist, timeout, body/header limits e SSRF guard.
- [ ] Cache/rate limit persistente usa provider duravel sem acoplar gateway a Turso.
- [ ] Historico e SSE expõem eventos seguros, filtraveis e com request ID.
- [ ] Host routing respeita reserved paths, namespace e isolamento por host.
- [ ] Todas as provas rodam localmente; `docker-compose` e permitido apenas para dependencia local.
- [ ] Gate de planejamento fica verde: `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`.

## Status

planned (2026-06-29) - criado como dono modular para proxy externo, cache, rate limit persistente/distribuido, SSE, historico e vhosts, preservando o Epic 08 como consolidacao.

