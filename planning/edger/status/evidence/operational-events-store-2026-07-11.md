# Evidência 2026-07-11: store local de eventos operacionais

## Contrato

- Store local em memória, com 2.000 eventos globais e 200 por identidade.
- Identidade por namespace, worker e versão; process ID é campo opcional do envelope.
- IDs monotônicos e paginação `before` retornam eventos newest-first sem duplicar após novas inserções.
- Filtros combináveis: tempo, namespace, worker, versão, source, level, outcome, status, request ID e trace ID.
- API `GET /api/admin/observability/events` exige root.
- Envelope serializa somente campos allowlisted; não possui body, headers ou env.
- Mensagens com indicadores de Authorization, cookie, password, secret, token, API key ou body são substituídas por `[redacted]`.
- O dispatch do pipeline registra sucesso, HTTP 5xx e falhas com o mesmo `x-request-id` devolvido ao cliente.

## TDD e custo

O primeiro teste falhou por ausência do módulo `observability`, do store e do acesso pelo `ServerState`. Após a implementação:

```text
cargo test -p edger-orchestrator --test observability_api -- --nocapture
4 passed; 0 failed
10k bounded event inserts: 865.013625ms
```

O teste cobre retenção determinística, cursor, filtros com isolamento de namespace/versão, root-only, redaction e correlação de um dispatch real.

## Tráfego real e Browser

Após requests reais para `boom-ui`, `commonjs` e `cpanel-scenario`, a API retornou cinco eventos, capacidade 2.000, zero evicted/dropped e outcomes `http_5xx`/`ok` com status e request IDs.

No Browser embutido, `/cpanel/workers/boom-ui/1.1.0/logs` mostrou:

- navegação segmentada Files / Observability / Logs;
- evento real `error · dispatch · http_5xx · 500`;
- duração e request ID correlacionável;
- aviso de retenção local/reset e independência de OTEL;
- filtro exato por request ID persistido na query string;
- refresh manteve rota, filtro e evento selecionado.

O runtime local permaneceu em execução na porta 19080 para continuidade da validação.
