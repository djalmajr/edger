# Story 08.16: Gateway rate limit em memória

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz ainda marca `Gateway/proxy rules` como `must partial`; depois de CORS/preflight e redirect rules, falta uma política de borda que bloqueie excesso de requests antes de atingir worker/redirect.
- **Objetivo:** Adicionar rate limiting em memória no `edger-ext-gateway`, com bucket por cliente, resposta `429` e headers operacionais.
- **Valor:** Operadores passam a ter uma proteção local básica contra bursts por cliente, aproximando o valor de gateway do Buntime sem copiar API/admin/persistência.
- **Restrições:** Sem persistência Turso, sem API admin de buckets, sem SSE/UI, sem regex `excludePaths`, sem rate-limit por usuário autenticado nesta fatia.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `docs/developers/06-operacao-e-testes.adoc`, `planning/edger/epics/08-valor-buntime/00-overview.md`
- **Buntime refs:** `wiki/apps/plugin-gateway.md` em `workspace: zommehq`, `project: buntime`; aprendizado aplicado: token bucket por cliente, CORS preflight antes de rate limit, `429` com `Retry-After` e headers de limite.
- **Prototype refs:** none; this is middleware/runtime behavior.
- **Business rules:** Paridade é por valor operacional observável; persistência e API de gestão são lacunas explícitas, não parte deste slice.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/src/lib.rs` | edit | Adicionar config e estado de rate limit em memória ao middleware |
| `edger-ext-gateway/tests/gateway_middleware.rs` | edit | Provar consumo por cliente, 429, headers, preflight não consumindo e ordem antes de redirect |
| `planning/edger/docs/value-parity-matrix.md` | edit | Atualizar evidência da linha `Gateway/proxy rules` |
| `planning/edger/docs/compat-matrix.md` | edit | Registrar compatibilidade técnica do rate limit v1 |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar rate limit local e lacunas restantes |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | edit | Adicionar 08.16 no backlog, roadmap e status |
| `planning/edger/status/evidence/story-08-16-runtime.txt` | create | Capturar comandos e resultados |

## Detail

### AS-IS
- O gateway aplica CORS/preflight e redirect rules.
- Requests em excesso por cliente ainda chegam a redirect/worker se o pipeline permitir.
- A linha `Gateway/proxy rules` permanece parcial por falta de proxy externo, cache e rate-limit persistente.

### TO-BE
- `GatewayRateLimitConfig` define capacidade e janela em segundos.
- `GatewayExtension::with_rate_limit` ativa um token bucket em memória por cliente.
- A chave padrão usa `X-Forwarded-For` primeiro IP, depois `X-Real-IP`, senão `unknown`.
- Preflight CORS continua respondendo `204` antes do rate limit e não consome bucket.
- Requests bloqueados retornam `429` com `x-ratelimit-limit`, `x-ratelimit-remaining: 0` e `retry-after`.
- Rate limit roda antes de redirect rules, então redirects também são protegidos.

### Scope
- **In:** token bucket em memória, key por IP/header, headers de bloqueio, testes de contrato.
- **Out:** persistência, API admin, listagem de buckets, SSE/UI, rate-limit por usuário autenticado, regex excludes, distributed sync.

### Approach
- Guardar buckets em `Mutex<HashMap<String, RateLimitBucket>>` dentro do gateway.
- Reabastecer tokens preguiçosamente com `Instant::now()` a cada request.
- Adicionar construtor `GatewayRateLimitConfig::new(max_requests, window_seconds)` e `with_key_header`.
- Inserir a decisão de rate limit depois de preflight e antes de redirect.

### Risks
- **Overclaiming:** O valor entregue é rate limit local em memória; a matriz deve continuar `partial` até existir persistência/API/distribuição.
- **Flakiness por tempo:** Testes devem usar capacidade baixa e não depender de sleep.
- **CORS regression:** Preflight precisa continuar independente do bucket.

### Acceptance criteria
- [x] Um cliente pode fazer `max_requests` dentro da janela e o próximo request recebe `429`.
- [x] `429` inclui `x-ratelimit-limit`, `x-ratelimit-remaining: 0` e `retry-after`.
- [x] Clientes diferentes têm buckets independentes.
- [x] CORS preflight não consome bucket.
- [x] Rate limit é aplicado antes de redirect rules.
- [x] Matriz e docs registram rate limit local como entrega parcial, mantendo persistência/API/proxy/cache como lacunas.
- [x] Gates Rust e planejamento passam.

## Test-first plan
- **Behavior:** token bucket local bloqueia o terceiro request quando `max_requests=2`.
- **First failing test:** adicionar teste em `gateway_middleware.rs` para dois requests permitidos e terceiro `429` com headers.
- **Preferred level:** teste de middleware, porque a política é uma extensão de borda antes do worker.
- **Mutation captured:** remover consumo do bucket, usar chave global única ou aplicar redirect antes do rate limit deve quebrar testes.
- **Avoid:** teste com relógio/sleep, rede externa ou API admin inexistente.

## Tasks
- [x] Fase 1 — Testes de contrato do rate limit.
  - Done when: testes falham sem os tipos/API novos.
- [x] Fase 2 — Implementação do token bucket local.
  - Done when: testes focados passam.
- [x] Fase 3 — Atualizar artefatos de valor.
  - Done when: overview, matriz, compat, docs e evidência refletem 08.16.
- [x] Fase 4 — Rodar gates.
  - Done when: Rust gate completo e planning gate passam.

## Verification
```bash
cargo test -p edger-ext-gateway --test gateway_middleware
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
