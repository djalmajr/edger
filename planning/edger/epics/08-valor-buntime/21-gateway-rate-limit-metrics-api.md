# Story 08.21: API de métricas de rate limit do gateway

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context

O buntime expõe uma superfície explícita para métricas de rate limit do gateway em `/rate-limit/metrics`. O edger já registra o estado agregado de rate limit dentro de `/api/admin/gateway/stats`, mas ainda não oferece um endpoint dedicado para esse contrato operacional.

Objetivo: expor um endpoint root-only e somente leitura para métricas agregadas de rate limit do gateway, sem listar buckets nem chaves de cliente.

Valor: operador consegue consultar rapidamente se o rate limit local está ativo, quantos buckets existem e qual é a configuração efetiva, mantendo a diferença de design do edger: nada de mutações dinâmicas ou exposição de identificadores de cliente nesta fatia.

Constraints:

- Não copiar a API completa do buntime como está; entregar o valor operacional mínimo com segurança.
- Não expor headers, tokens, IPs brutos em endpoints de métricas dedicadas.
- Rate limit segue local/single-node nesta história; persistência/distribuição continuam lacuna explícita.

## Traceability

- Fonte Buntime: `wiki/apps/plugin-gateway.md`, seção Rate limiting, `GET /rate-limit/metrics`.
- Matriz: `planning/edger/docs/value-parity-matrix.md`, linha Gateway/proxy rules.
- Dependências: 08.16, 08.17, 08.18.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/admin_api.rs` | alter | Adicionar rota e handler root-only para métricas agregadas de rate limit. |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | alter | Cobrir contrato HTTP, auth root e ausência de vazamento de segredo. |
| `docs/developers/06-operacao-e-testes.adoc` | alter | Documentar curl e escopo local/single-node do endpoint. |
| `planning/edger/docs/value-parity-matrix.md` | alter | Atualizar evidência e lacunas restantes da linha gateway. |
| `planning/edger/docs/compat-matrix.md` | alter | Registrar contrato de compatibilidade operacional. |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | alter | Inserir a história no backlog e roadmap. |
| `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md` | alter | Atualizar status e evidência do Epic 08. |
| `README.md` e `planning/edger/roadmap.md` | alter | Atualizar resumo e contagem de stories. |
| `planning/edger/status/evidence/story-08-21-runtime.txt` | create | Registrar comandos e resultados de verificação. |
| `planning/edger/status/closure-2026-06-29-story-08-21-gateway-rate-limit-metrics-api.md` | create | Fechar a entrega da história. |

## Detail

AS-IS:

- `/api/admin/gateway/stats` inclui `rateLimit`, mas o operador precisa buscar o snapshot completo.
- `/api/admin/gateway/logs/stats` cobre logs, não bucket/config rate-limit.
- Buckets existem internamente e podem conter identificadores sensíveis dependendo do keying.

TO-BE:

- `GET /api/admin/gateway/rate-limit/metrics` exige root e retorna somente dados agregados:
  - `enabled`
  - `activeBuckets`
  - `maxRequests` quando habilitado
  - `windowSeconds` quando habilitado
  - `scope: "local-memory"`
- Sem buckets individuais, reset, clear ou mutação.

Scope:

- Inclui endpoint read-only agregado, teste e documentação.
- Exclui `/rate-limit/buckets`, `DELETE /buckets/:key`, `POST /clear`, persistência e distribuição.

### Acceptance criteria

- `GET /api/admin/gateway/rate-limit/metrics` retorna `401` sem credencial root.
- `GET /api/admin/gateway/rate-limit/metrics` retorna `200` com root e JSON agregado do rate limit local.
- A resposta inclui `scope: "local-memory"` para deixar claro que a entrega não é distribuída/persistente.
- A resposta não serializa headers sensíveis, authorization, IPs ou chaves de bucket.
- Matriz, compatibilidade, runbook e checkpoint preservam lacunas de buckets/reset, persistência/distribuição, SSE/histórico e mutações dinâmicas.

Risks:

- Expor chaves de bucket pode vazar IP/usuário/tenant. Mitigação: não listar buckets nesta história.
- Duplicar cálculo já existente. Mitigação: reutilizar `diagnostics["rateLimit"]` como fonte única.

## Test-first plan

Behavior:

- Sem API key, o endpoint retorna `401`.
- Com root key, o endpoint retorna `200` e JSON agregado do rate limit.
- O JSON não contém headers sensíveis nem authorization.

First failing test:

- `gateway_admin_rate_limit_metrics_api_exposes_local_bucket_summary` em `edger-orchestrator/tests/admin_workers_plugins.rs`.

Preferred level:

- Integração HTTP via `build_pipeline`, porque o contrato é status + JSON + auth root.

Low-value tests to avoid:

- Testar função auxiliar isolada sem passar pelo router.
- Testar buckets internos ou nomes de chaves, porque isso fixa implementação e cria risco de exposição.

## Tasks

- [x] Criar teste de contrato HTTP root-only.
- [x] Adicionar rota e handler read-only no Admin API.
- [x] Documentar endpoint e escopo local.
- [x] Atualizar matriz, overview, roadmap, checkpoint e evidência.
- [x] Rodar teste focado, suite workspace, clippy, fmt e gate de planejamento.

## Verification

```bash
cargo test -p edger-orchestrator --test admin_workers_plugins gateway_admin_rate_limit_metrics_api_exposes_local_bucket_summary
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
git diff --check
```
