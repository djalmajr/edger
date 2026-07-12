# Story 21.03: Eventos operacionais unificados

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** eventos `tracing`, erros por worker e decisões do gateway têm fontes e retenções diferentes; apenas parte é consultável.
- **Objective:** criar envelope e store bounded para eventos operacionais consultáveis pela Admin API.
- **Value:** fornece uma fonte segura para detalhe, logs e correlação; console dos workers entra depois por adapter controlado na 21.07.
- **Constraints:** `edger-core` não recebe I/O; hot path deve manter custo previsível; root-only e allowlist de campos.

## Traceability

- **Prototype:** contrato alimenta `OBS-02`, `OBS-03` e `OBS-04`.
- **Business rules:** identidade por namespace/name/version; `requestId` opcional mas indexável; sem body/header/env/segredo.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/observability.rs` | create | Store bounded e filtros |
| `edger-orchestrator/src/server.rs` | edit | Ownership do store |
| `edger-orchestrator/src/pipeline.rs` | edit | Registrar dispatch/error allowlisted |
| `edger-orchestrator/src/admin_api.rs` | edit | API cursor/filtros root-only |
| `edger-orchestrator/tests/observability_api.rs` | create | Contrato, limites e redaction |
| `planning/edger/docs/compat-matrix.md` | edit | Registrar superfície consultável |

## Detail

### AS-IS

- `WorkerErrorLog` guarda 20 erros por nome; `edger.dispatch` vai ao tracing; gateway possui ring próprio.

### TO-BE

- `OperationalEvent` contém `id`, `atMs`, `source`, `kind`, `level`, identidade do worker, `requestId`, outcome, status, duration e mensagem sanitizada opcional.
- Store aplica limite global + por identidade, monotonic cursor e contadores de dropped/evicted.
- `GET /api/admin/observability/events` filtra por cursor/time/worker/version/source/level/outcome/status/requestId.
- Gateway entra por adapter explícito ou permanece fonte separada até contrato compatível; nenhuma duplicação silenciosa.

### Scope

- **In:** memória bounded, API read-only, filtros, paginação cursor, redaction e métricas do store.
- **Out:** disco, full-text, captura de stdout/stderr (21.07), OTLP exporter (21.08) e SSE (21.06).

### Acceptance criteria

- [x] Limites globais/por-worker são determinísticos e testados.
- [x] Filtros combinam sem perder isolamento de namespace/versão.
- [x] API nunca serializa segredo, headers ou bodies.
- [x] Eventos mantêm correlação com response `x-request-id` quando disponível.
- [x] Custo do registro é medido e não bloqueia dispatch.

### Dependencies

- Nenhuma; é o contrato crítico do epic.

## Tasks

- [x] Escrever testes de schema/redaction/retention antes do store.
- [x] Implementar store e ownership no server state.
- [x] Conectar dispatch e operational errors sem duplicar tracing.
- [x] Expor API com cursor/filtros e auth.
- [x] Medir custo, atualizar docs e gates.

## Verification

```bash
cargo test -p edger-orchestrator --test observability_api
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

completed (2026-07-11) — envelope allowlisted, store bounded, cursor/filtros, API root-only e registro do dispatch entregues. Evidência: `planning/edger/status/evidence/operational-events-store-2026-07-11.md`.
