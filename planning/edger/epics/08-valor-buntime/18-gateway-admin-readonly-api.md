# Story 08.18: API admin read-only do gateway

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A Story 08.17 tornou o diagnóstico do gateway visível no inventário de extensões, mas o operador ainda precisa conhecer a estrutura de `/api/admin/extensions` para achar stats/logs/config. Buntime entrega rotas dedicadas para stats, logs e config do gateway.
- **Objetivo:** Adicionar endpoints root-only em `/api/admin/gateway/*` para stats, logs filtráveis e config read-only, reaproveitando o diagnóstico local do gateway.
- **Valor:** Operadores passam a consultar o gateway como recurso operacional próprio, sem UI, SSE, persistência ou mutações dinâmicas.
- **Restrições:** Sem `/gateway/api/*`, sem SSE, sem histórico persistente, sem clear/delete logs, sem mutação de config, sem cache/proxy externo.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `docs/developers/06-operacao-e-testes.adoc`
- **Buntime refs:** `wiki/apps/plugin-gateway.md` em `workspace: zommehq`, `project: buntime`; aprendizado aplicado: stats/config/logs são uma superfície operacional dedicada, mas persistência e SSE são capacidades separadas.
- **Prototype refs:** none; this is Admin API/runtime observability behavior.
- **Business rules:** Endpoints são root-only, read-only e não podem expor headers, body, API keys ou segredos.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/src/lib.rs` | edit | Adicionar `config` seguro ao snapshot de diagnóstico |
| `edger-ext-gateway/tests/gateway_middleware.rs` | edit | Provar config seguro no diagnóstico |
| `edger-orchestrator/src/admin_api.rs` | edit | Adicionar `/api/admin/gateway/stats`, `/logs` e `/config` |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar auth root-only, stats, config e filtros de logs |
| `planning/edger/docs/value-parity-matrix.md` | edit | Atualizar evidência de gateway/logging |
| `planning/edger/docs/compat-matrix.md` | edit | Registrar API admin read-only do gateway |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar os endpoints e limites |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | edit | Adicionar 08.18 no backlog, roadmap e status |
| `planning/edger/status/evidence/story-08-18-runtime.txt` | create | Capturar comandos e resultados |

## Detail

### AS-IS
- Gateway expõe diagnóstico opcional embutido em `/api/admin/extensions`.
- O snapshot não tem bloco `config` separado.
- Logs recentes não têm endpoint próprio nem filtros.

### TO-BE
- `GET /api/admin/gateway/stats` retorna o snapshot de diagnóstico completo.
- `GET /api/admin/gateway/config` retorna apenas a configuração segura do gateway.
- `GET /api/admin/gateway/logs?limit=&rateLimited=&status=&decision=` retorna decisões recentes filtradas.
- Todos os endpoints exigem root auth existente.
- Config expõe CORS, contagem de redirect rules e rate-limit config, sem destinos de redirect ou dados sensíveis.

### Scope
- **In:** endpoints read-only, filtros em memória, limite de retorno, config seguro.
- **Out:** SSE, persistência, limpar logs, reset de buckets, alteração de config, endpoint público `/gateway/api/*`, proxy/cache.

### Approach
- Reaproveitar `registry.admin_extension("gateway").diagnostics`.
- Manter a API dedicada no `admin_api`, protegida por `require_root`.
- Filtrar `recentDecisions` no handler sem acoplar o orchestrator ao tipo concreto do gateway.
- Adicionar `config` ao diagnóstico do gateway para o endpoint dedicado não inferir shape a partir de detalhes soltos.

### Risks
- **Overclaiming:** Não declarar paridade de SSE/histórico; a entrega é API local read-only.
- **Secret leakage:** Config não deve incluir headers, bodies, raw keys nem target completo de redirects.
- **Shape drift:** Endpoints devem usar o snapshot existente para evitar duplicar fonte de verdade.

### Acceptance criteria
- [x] `/api/admin/gateway/stats` exige root e retorna requests/rateLimit/recentDecisions/config.
- [x] `/api/admin/gateway/config` retorna config seguro de CORS, redirect rule count e rate limit.
- [x] `/api/admin/gateway/logs` suporta `limit`, `rateLimited`, `status` e `decision`.
- [x] Logs filtrados não incluem headers, bodies ou segredos.
- [x] Matriz e docs registram API read-only, mantendo SSE/persistência/mutações como lacunas.
- [x] Gates Rust e planejamento passam.

## Test-first plan
- **Behavior:** operador root consulta stats/config/logs do gateway sem atravessar o inventário genérico de extensões.
- **First failing test:** adicionar teste em `admin_workers_plugins.rs` chamando `/api/admin/gateway/stats` antes das rotas existirem.
- **Preferred level:** teste de Admin API para contrato HTTP + teste de middleware para shape do diagnóstico.
- **Mutation captured:** remover `require_root`, ignorar filtros ou vazar segredo em logs/config deve quebrar testes.
- **Avoid:** testar SSE, persistência, reset de buckets ou mutações de config inexistentes.

## Tasks
- [x] Fase 1 — Testes de API read-only.
  - Done when: testes falham sem as rotas e sem `config` no diagnóstico.
- [x] Fase 2 — Config seguro no diagnóstico do gateway.
  - Done when: snapshot inclui `config` sem segredos.
- [x] Fase 3 — Endpoints Admin API.
  - Done when: stats/config/logs passam com root auth e filtros.
- [x] Fase 4 — Atualizar artefatos de valor.
  - Done when: overview, matriz, compat, docs e evidência refletem 08.18.
- [x] Fase 5 — Rodar gates.
  - Done when: Rust gate completo e planning gate passam.

## Verification
```bash
cargo test -p edger-ext-gateway --test gateway_middleware
cargo test -p edger-orchestrator --test admin_workers_plugins gateway_admin_readonly_api_exposes_stats_config_and_filtered_logs -- --exact
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
