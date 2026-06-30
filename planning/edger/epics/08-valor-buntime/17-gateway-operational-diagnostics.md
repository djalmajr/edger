# Story 08.17: Diagnóstico operacional do gateway

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** Depois de CORS/preflight, redirects e rate limit local, o gateway já toma decisões de borda, mas o operador ainda não consegue inspecionar essas decisões pela superfície operacional existente.
- **Objetivo:** Expor um snapshot de diagnóstico protegido no inventário da Admin API de extensões, com contadores e decisões recentes do `edger-ext-gateway`.
- **Valor:** Aproxima o valor operacional do Buntime, que combina gateway policy com stats/logs, sem copiar UI, SSE, storage ou rotas específicas de plugin.
- **Restrições:** Sem endpoint `/gateway/api/*`, sem SSE, sem persistência, sem log global de responses de worker, sem cache/proxy externo.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `docs/developers/06-operacao-e-testes.adoc`
- **Buntime refs:** `wiki/apps/plugin-gateway.md` em `workspace: zommehq`, `project: buntime`; aprendizado aplicado: request log ring buffer e stats são valor operacional do gateway, mas persistência/SSE/API dinâmica ficam como fatias futuras.
- **Prototype refs:** none; this is Admin API/runtime observability behavior.
- **Business rules:** Diagnóstico não pode vazar headers, body, API keys ou segredos; deve permanecer protegido pela Admin API root já existente.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/extension.rs` | edit | Adicionar contrato puro de diagnóstico opcional por extensão |
| `edger-core/src/admin.rs` | edit | Permitir `diagnostics` opcional no inventário de extensões |
| `edger-orchestrator/src/registry.rs` | edit | Agregar diagnóstico opcional no `AdminExtensionInfo` |
| `edger-ext-gateway/src/lib.rs` | edit | Manter contadores e ring buffer local das decisões do gateway |
| `edger-ext-gateway/tests/gateway_middleware.rs` | edit | Provar snapshot, contadores, ring buffer e higiene de dados |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar que `/api/admin/extensions` expõe diagnóstico do gateway sem quebrar auth |
| `planning/edger/docs/value-parity-matrix.md` | edit | Atualizar evidência de gateway/logging |
| `planning/edger/docs/compat-matrix.md` | edit | Registrar diagnóstico técnico do gateway |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar snapshot operacional e limites |
| `planning/edger/status/evidence/story-08-17-runtime.txt` | create | Capturar comandos e resultados |

## Detail

### AS-IS
- O gateway short-circuita preflight, redirect e rate limit, mas não mantém uma trilha operacional consultável.
- `/api/admin/extensions` lista nome/capabilities/status, sem detalhes de runtime.
- A matriz ainda deixa logging acionável e API dinâmica de gateway como lacunas explícitas.

### TO-BE
- `Extension::diagnostics()` retorna `Option<serde_json::Value>` por padrão `None`.
- `AdminExtensionInfo` inclui `diagnostics` apenas quando a extensão fornece snapshot.
- `GatewayExtension` expõe contadores de decisões e últimas 100 decisões locais.
- Cada entrada recente contém `requestId`, `method`, `path`, `decision`, `status`, `client` e `rateLimited`, sem body ou headers brutos.
- `/api/admin/extensions` continua root-only e passa a mostrar o snapshot do gateway.

### Scope
- **In:** diagnóstico em memória, ring buffer local, contadores por decisão, serialização na Admin API de extensões.
- **Out:** SSE, histórico persistente, filtros de log, avg duration, API `/gateway/api/logs`, proxy/cache, rate-limit distribuído.

### Approach
- Adicionar método default `diagnostics()` no trait `Extension`.
- Agregar o snapshot no registry sem downcast e sem acoplamento do orchestrator ao tipo concreto do gateway.
- Registrar decisões no `on_request`, incluindo `continue`, `preflight`, `redirect` e `rate_limited`.
- Limitar o ring buffer a 100 entradas para manter memória previsível.

### Risks
- **Overclaiming:** Esta story entrega diagnóstico local do gateway, não logs globais nem SSE.
- **Secret leakage:** O snapshot deve evitar headers e body; o campo `client` usa IP derivado ou `unknown`, não credenciais.
- **Trait creep:** O método deve ser opcional e genérico para não transformar todas as extensões em providers operacionais obrigatórias.

### Acceptance criteria
- [x] `GatewayExtension::diagnostics()` retorna contadores coerentes após decisões `continue`, `preflight`, `redirect` e `rate_limited`.
- [x] O ring buffer preserva no máximo 100 decisões recentes.
- [x] O snapshot não inclui headers, body, `authorization`, `x-api-key` ou valores sensíveis.
- [x] `/api/admin/extensions` retorna `diagnostics` para `gateway` sob root auth.
- [x] Matriz e docs registram o valor como diagnóstico local, mantendo SSE/persistência/API dinâmica como lacunas.
- [x] Gates Rust e planejamento passam.

## Test-first plan
- **Behavior:** operador root lista extensões e vê diagnóstico do gateway; testes de middleware provam contadores e ring buffer.
- **First failing test:** adicionar teste em `gateway_middleware.rs` chamando `diagnostics()` antes do método existir.
- **Preferred level:** teste de contrato do middleware + teste de Admin API para serialização protegida.
- **Mutation captured:** deixar de registrar `429`, remover limite do ring buffer ou serializar headers brutos deve quebrar testes.
- **Avoid:** testar logs globais de workers, duração média, SSE ou persistência inexistentes.

## Tasks
- [x] Fase 1 — Testes de diagnóstico do gateway.
  - Done when: testes falham sem `diagnostics`/snapshot.
- [x] Fase 2 — Contrato opcional no core e registry.
  - Done when: `/api/admin/extensions` serializa diagnóstico opcional.
- [x] Fase 3 — Implementação do snapshot no gateway.
  - Done when: testes focados passam.
- [x] Fase 4 — Atualizar artefatos de valor.
  - Done when: overview, matriz, compat, docs e evidência refletem 08.17.
- [x] Fase 5 — Rodar gates.
  - Done when: Rust gate completo e planning gate passam.

## Verification
```bash
cargo test -p edger-ext-gateway --test gateway_middleware
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
