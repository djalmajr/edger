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
- Proxy loopback local existe como primeira fatia funcional.
- Vhosts/host routing local existe via `hosts` no manifesto.
- Cache, SSE/historico operacional e mutacoes dinamicas ainda nao foram fechados.

### TO-BE

- Proxy evolui de loopback local testavel para politicas de allowlist mais amplas, limites, timeout e protecoes contra SSRF.
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
| 11.01 Proxy forwarding local | `01-proxy-forwarding-local.md` | Encaminhar requests para upstreams loopback permitidos com timeout e SSRF guard | large | completed | Epic 08.15, Epic 08.18 |
| 11.02 Cache e rate limit persistente | `02-cache-rate-limit-persistente.md` | Persistir cache/rate limit quando provider duravel estiver configurado | large | planned | 11.01, Epic 09 |
| 11.03 Historico e SSE operacional | `03-historico-sse-operacional.md` | Expor historico operacional e stream local de eventos seguros | medium | planned | 11.01, Epic 09 |
| 11.04 Vhosts e host routing | `04-vhosts-host-routing.md` | Resolver apps por host sem route hijack e com evidencia local | medium | completed | 11.01 |

## Epic acceptance criteria

- [x] Proxy local valida loopback allowlist, timeout e SSRF guard.
- [ ] Cache/rate limit persistente usa provider duravel sem acoplar gateway a Turso.
- [ ] Historico e SSE expõem eventos seguros, filtraveis e com request ID.
- [x] Host routing respeita reserved paths, namespace e isolamento por host.
- [ ] Todas as provas rodam localmente; `docker-compose` e permitido apenas para dependencia local.
- [ ] Gate de planejamento fica verde: `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`.

## Status

in-progress (2026-07-01) - Story 11.01 entregou proxy HTTP loopback-only em `edger-ext-gateway`, com timeout, sanitizacao de headers sensiveis, diagnostics e teste de upstream controlado. Story 11.04 entregou vhosts/host routing local via `hosts` no manifesto, com reserved paths protegidos e prova por header `Host`. Cache/rate limit persistente, SSE/historico operacional e mutacoes dinamicas continuam como stories dedicadas.
