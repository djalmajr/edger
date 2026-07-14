# Story 08.20: duração real nos logs do gateway

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A Story 08.19 agregou stats de logs, mas ainda declarava `duration.tracked=false`. Buntime expõe duração média em `/logs/stats`, e isso é valor operacional para detectar latência/pressão do gateway.
- **Objetivo:** Registrar `durationMs` nas decisões recentes do gateway e calcular `duration.avgMs` em `/api/admin/gateway/logs/stats` quando houver amostras.
- **Valor:** Operadores conseguem diferenciar volume de requests de lentidão real sem baixar logs brutos, UI, SSE ou histórico persistente.
- **Restrições:** Sem histograma/p95, sem persistência, sem SSE, sem log clear/delete, sem mutações de gateway.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `docs/developers/06-operacao-e-testes.adoc`
- **Buntime refs:** `wiki/apps/plugin-gateway.md` em `workspace: zommehq`, `project: buntime`; aprendizado aplicado: `/logs/stats` inclui duração média, mas Edger mantém a medição local e read-only.
- **Prototype refs:** none; this is Admin API/runtime observability behavior.
- **Business rules:** Logs continuam sem headers, body, API keys, raw secrets ou payloads de worker.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/src/lib.rs` | edit | Registrar `durationMs` nas decisões e atualizar entrada em `on_response` |
| `edger-ext-gateway/tests/gateway_middleware.rs` | edit | Provar duração em short-circuit e response hook |
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Calcular `duration.avgMs` e `samples` em `/logs/stats` |
| `crates/edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar stats com duração rastreada |
| `planning/edger/docs/value-parity-matrix.md` | edit | Remover duração média real da lacuna de logging |
| `planning/edger/docs/compat-matrix.md` | edit | Registrar duração em logs/stats |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar `duration.avgMs`/`samples` |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | edit | Adicionar 08.20 no backlog, roadmap e status |
| `planning/edger/status/evidence/story-08-20-runtime.txt` | create | Capturar comandos e resultados |

## Detail

### AS-IS
- `recentDecisions` registra decisão, status, request ID, método, path, cliente e rate-limit.
- `/api/admin/gateway/logs/stats` agrega contagens, mas `duration.tracked=false`.
- Entradas `continue` não recebem status/duração depois do worker responder.

### TO-BE
- Short-circuits do gateway registram `durationMs` no momento da decisão.
- Requests que continuam para worker são atualizados em `on_response` com status final e `durationMs`.
- `/api/admin/gateway/logs/stats` retorna `duration.tracked=true`, `samples` e `avgMs` quando há ao menos uma amostra.
- Se não houver amostras, o shape antigo permanece explícito: `tracked=false`, `samples=0`, `avgMs=null`.

### Scope
- **In:** duração local em ms, status final para entradas `continue`, média simples em stats.
- **Out:** histograma, percentis, persistência, SSE, histórico durável, mutações dinâmicas.

### Approach
- Usar `RequestContext::start` como fonte de tempo sem alterar `edger-core`.
- Manter a fonte única em `GatewayDiagnostics`.
- Atualizar a entrada existente por `requestId` em `on_response`.
- Calcular a média somente a partir de entradas com `durationMs` numérico.

### Risks
- **Shape drift:** Manter `durationMs` como campo adicional nas entradas, sem remover os campos existentes.
- **Overclaiming:** Não declarar p95/histórico/SSE como entregues.
- **Secret leakage:** Não copiar request/response body, headers ou detalhes de worker.

### Acceptance criteria
- [x] Decisões short-circuit (`preflight`, `redirect`, `rate_limited`) incluem `durationMs`.
- [x] Decisões `continue` recebem status e `durationMs` após `on_response`.
- [x] `/api/admin/gateway/logs/stats` calcula `duration.tracked`, `samples` e `avgMs`.
- [x] Logs/stats continuam sem headers, bodies, authorization ou segredo bruto.
- [x] Matriz e docs registram duração real como entregue, mantendo SSE/histórico/persistência como lacunas.
- [x] Gates Rust e planejamento passam.

## Test-first plan
- **Behavior:** operador vê duração média real dos logs recentes do gateway quando o runtime executa hooks de request/response.
- **First failing test:** adicionar teste no gateway esperando `durationMs` em `recentDecisions` depois de `on_response`.
- **Preferred level:** teste unitário de middleware para diagnóstico + teste Admin API para agregação HTTP.
- **Mutation captured:** remover atualização em `on_response` ou ignorar `durationMs` no agregador deve quebrar testes.
- **Avoid:** testar percentis, persistência, SSE ou latência exata maior que zero.

## Tasks
- [x] Fase 1 — Testes de duração.
  - Done when: testes falham sem `durationMs` e sem stats rastreados.
- [x] Fase 2 — Implementar medição local.
  - Done when: gateway registra/amplia entradas sem vazar dados sensíveis.
- [x] Fase 3 — Atualizar agregação e docs.
  - Done when: `/logs/stats` calcula média e artefatos refletem a 08.20.
- [x] Fase 4 — Rodar gates.
  - Done when: Rust gate completo e planning gate passam.

## Verification
```bash
cargo test -p edger-ext-gateway --test gateway_middleware diagnostics_records_response_duration_without_sensitive_data -- --exact
cargo test -p edger-orchestrator --test admin_workers_plugins gateway_admin_gateway_log_stats_api_aggregates_recent_decisions -- --exact
cargo test -p edger-ext-gateway --test gateway_middleware
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
