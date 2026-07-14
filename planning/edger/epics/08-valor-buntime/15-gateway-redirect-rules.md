# Story 08.15: Gateway redirect rules

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz marca `Gateway/proxy rules` como `must partial`; o gateway atual cobre CORS/preflight, mas ainda não tem uma regra de borda declarativa que redirecione requests antes do worker.
- **Objetivo:** Entregar redirect rules ordenadas e testáveis em `edger-ext-gateway`, preservando path suffix e query string.
- **Valor:** Operadores ganham uma capacidade mínima de roteamento de borda verificável, aproximando o valor de proxy/redirect do Buntime sem copiar o `plugin-proxy` nem prometer forwarding externo nesta fatia.
- **Restrições:** Sem proxy HTTP upstream, cache, rate-limit persistente, API admin dinâmica ou storage durável nesta story.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `docs/developers/06-operacao-e-testes.adoc`, `planning/edger/epics/08-valor-buntime/00-overview.md`
- **Buntime refs:** `wiki/apps/plugin-gateway.md` e `wiki/ops/runbook-apps-gateway-proxy.md` em `workspace: zommehq`, `project: buntime`; aprendizado aplicado: regras de borda precisam ser declarativas, ordenadas e verificáveis, enquanto proxy externo e persistência dinâmica ficam como capacidades separadas.
- **Prototype refs:** none; this is middleware/runtime behavior.
- **Business rules:** Paridade é por valor operacional observável, não por copiar o plugin ou sua API.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/src/lib.rs` | edit | Adicionar `GatewayRedirectRule` e short-circuit de redirect no middleware |
| `edger-ext-gateway/tests/gateway_middleware.rs` | edit | Provar redirect, preservação de suffix/query e precedência de CORS preflight |
| `crates/edger-orchestrator/src/wire.rs` | edit | Preservar query string em `SerializedRequest.uri`, requisito para redirect/proxy correto |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Preservar query string depois do rewrite de path para o worker |
| `planning/edger/docs/value-parity-matrix.md` | edit | Atualizar evidência da linha `Gateway/proxy rules` |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar gateway v1.1 com redirects e limites ainda fora de escopo |
| `planning/edger/status/evidence/story-08-15-runtime.txt` | create | Capturar comandos e resultados |

## Detail

### AS-IS
- `GatewayExtension` só incrementa contador de teste e responde CORS preflight.
- A matriz registra gateway/proxy como parcial por falta de proxy externo/cache/rate-limit persistente.
- `edger-orchestrator::wire` serializa apenas `path()`, descartando query string.

### TO-BE
- `GatewayRedirectRule` representa regra de prefixo com destino e status HTTP 301/302/307/308.
- `GatewayExtension::with_redirect_rules` aceita regras ordenadas; a primeira regra que casa short-circuita o request com `Location`.
- Redirect preserva o sufixo do path e a query string recebida.
- CORS preflight continua respondendo 204 antes de redirect para não quebrar browsers.
- `SerializedRequest.uri` preserva `path?query`.
- O pipeline mantém a query depois de reescrever `/worker/path?x=1` para o path relativo do worker.

### Scope
- **In:** redirect por prefixo, status permitido, preservação de suffix/query, teste unitário de middleware e teste de wire.
- **Out:** proxy reverse HTTP, rewrite regex, dynamic admin API, persistence, cache, rate-limit, vhosts.

### Approach
- Adicionar tipo público `GatewayRedirectRule` com construtor e status default 308.
- Normalizar prefixos para path absoluto e evitar que `/api` case `/apix`.
- Reutilizar `SerializedResponse` com header `location`.
- Corrigir o wire para usar `parts.uri.path_and_query()` quando existir.

### Risks
- **Overclaiming:** Redirect não é proxy. A matriz deve permanecer `partial` e listar proxy/cache/rate-limit persistente como futuras fatias.
- **CORS regression:** Preflight deve continuar vencendo redirect.
- **Path matching ambíguo:** Prefixo deve casar segmento (`/api` casa `/api` e `/api/users`, não `/apiary`).

### Acceptance criteria
- [x] Regra `/api -> https://backend.example.com/api` redireciona `/api/users?active=1` para `https://backend.example.com/api/users?active=1`.
- [x] Prefixo `/api` não casa `/apiary`.
- [x] `OPTIONS` com `Origin` continua retornando 204 CORS mesmo quando path casa redirect.
- [x] `axum_to_serialized` preserva query string.
- [x] Dispatch de worker preserva query depois do rewrite de base path.
- [x] Matriz e docs registram redirect rules como entrega parcial, mantendo proxy externo/cache/rate-limit persistente explícitos como lacuna.
- [x] Gates Rust e planejamento passam.

## Test-first plan
- **Behavior:** redirect de gateway short-circuita request não-preflight com status 308 e `Location`.
- **First failing test:** adicionar teste em `gateway_middleware.rs` para `/api/users?active=1` antes de implementar a regra.
- **Preferred level:** teste de middleware para regra de borda e teste unitário de `wire` para query string.
- **Mutation captured:** remover preservação de query ou permitir match `/apiary` deve quebrar testes.
- **Avoid:** teste com proxy real ou rede externa; esta story não implementa upstream forwarding.

## Tasks
- [x] Fase 1 — Testes de redirect e query.
  - Done when: testes falham sem implementação.
- [x] Fase 2 — Implementação gateway + wire.
  - Done when: testes focados passam.
- [x] Fase 3 — Atualizar artefatos de valor.
  - Done when: overview, matriz, docs e evidência refletem 08.15.
- [x] Fase 4 — Rodar gates.
  - Done when: Rust gate completo e planning gate passam.

## Verification
```bash
cargo test -p edger-ext-gateway --test gateway_middleware
cargo test -p edger-orchestrator wire::tests::roundtrip_preserves_method_path_query_headers_body
cargo test -p edger-orchestrator pipeline::tests::worker_request_preserves_query_after_path_rewrite
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
