# Story 08.19: stats agregados dos logs do gateway

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A Story 08.18 expôs logs filtráveis do gateway, mas o operador ainda precisa agregar manualmente `rateLimited`, status e decisões para entender a pressão recente do gateway. Buntime entrega `/logs/stats` como uma superfície operacional separada.
- **Objetivo:** Adicionar `GET /api/admin/gateway/logs/stats` root-only e read-only, calculado sobre `recentDecisions` do diagnóstico local do gateway.
- **Valor:** Operadores conseguem ver rapidamente volume retido, bloqueios por rate limit e distribuição por status/decisão sem UI, SSE ou persistência.
- **Restrições:** Sem histórico persistente, sem duração média real, sem clear/delete logs, sem reset de buckets, sem mutação de configuração.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `docs/developers/06-operacao-e-testes.adoc`
- **Buntime refs:** `wiki/apps/plugin-gateway.md` em `workspace: zommehq`, `project: buntime`; aprendizado aplicado: `/logs/stats` agrega logs operacionais, mas duração média depende de medição de resposta ainda fora desta fatia.
- **Prototype refs:** none; this is Admin API/runtime observability behavior.
- **Business rules:** Endpoint é root-only, read-only e não pode expor headers, body, API keys ou segredos.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Adicionar endpoint `/api/admin/gateway/logs/stats` e agregador |
| `crates/edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar auth root-only, agregados e higiene de segredos |
| `planning/edger/docs/value-parity-matrix.md` | edit | Atualizar evidência de logging/gateway |
| `planning/edger/docs/compat-matrix.md` | edit | Registrar endpoint de stats de logs |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar endpoint e limites |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | edit | Adicionar 08.19 no backlog, roadmap e status |
| `planning/edger/status/evidence/story-08-19-runtime.txt` | create | Capturar comandos e resultados |

## Detail

### AS-IS
- `/api/admin/gateway/logs` retorna entradas recentes e filtros.
- O operador calcula totais, bloqueios e distribuição por status manualmente.
- Não existe contrato dedicado equivalente ao valor operacional de `/logs/stats`.

### TO-BE
- `GET /api/admin/gateway/logs/stats` retorna agregados dos logs retidos:
  - `total`
  - `rateLimited`
  - `byStatus`
  - `byDecision`
  - `duration.tracked=false` e `duration.avgMs=null`
- Endpoint exige root auth existente.
- Agregação usa apenas `recentDecisions` do diagnóstico, preservando a fonte única.

### Scope
- **In:** endpoint read-only, agregação em memória, root auth, docs e matriz.
- **Out:** medição real de duração, SSE, persistência, limpar logs, reset de buckets, mutações de config.

### Approach
- Adicionar rota dedicada no `admin_api`.
- Reaproveitar `gateway_diagnostics(&state)`.
- Agregar `recentDecisions` sem depender de `GatewayExtension` concreto.
- Tratar `status: null` como `withoutStatus`, sem inventar HTTP status para decisões `continue`.

### Risks
- **Overclaiming:** Não declarar `avgDuration` como entregue; expor explicitamente que duração ainda não é rastreada.
- **Shape drift:** Usar `recentDecisions` como fonte única para manter stats e logs coerentes.
- **Secret leakage:** Agregados não devem copiar entradas brutas para dentro do endpoint.

### Acceptance criteria
- [x] `/api/admin/gateway/logs/stats` exige root.
- [x] Endpoint retorna `total`, `rateLimited`, `byStatus`, `byDecision` e `withoutStatus`.
- [x] Endpoint declara duração como não rastreada (`duration.tracked=false`).
- [x] Resposta não inclui headers, bodies, authorization ou segredo bruto.
- [x] Matriz e docs registram stats de logs, mantendo SSE/persistência/duração real como lacunas.
- [x] Gates Rust e planejamento passam.

## Test-first plan
- **Behavior:** operador root consulta estatísticas agregadas de logs do gateway sem baixar e agregar manualmente o ring buffer.
- **First failing test:** estender `gateway_admin_readonly_api_exposes_stats_config_and_filtered_logs` ou adicionar teste dedicado chamando `/api/admin/gateway/logs/stats` antes da rota existir.
- **Preferred level:** teste de Admin API para contrato HTTP e root auth.
- **Mutation captured:** remover `require_root`, ignorar status nulo ou vazar entradas brutas deve quebrar testes.
- **Avoid:** testar SSE, persistência, clear logs, reset de buckets ou duração real inexistente.

## Tasks
- [x] Fase 1 — Teste de contrato `/logs/stats`.
  - Done when: teste falha sem a rota e sem agregador.
- [x] Fase 2 — Implementar agregador read-only.
  - Done when: endpoint passa com root auth e shape esperado.
- [x] Fase 3 — Atualizar artefatos de valor.
  - Done when: overview, matriz, compat, docs e evidência refletem 08.19.
- [x] Fase 4 — Rodar gates.
  - Done when: Rust gate completo e planning gate passam.

## Verification
```bash
cargo test -p edger-orchestrator --test admin_workers_plugins gateway_admin_gateway_log_stats_api_aggregates_recent_decisions -- --exact
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
