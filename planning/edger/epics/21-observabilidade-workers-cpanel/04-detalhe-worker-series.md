# Story 21.04: Detalhe de worker e séries limitadas

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** o inventário mostra snapshot por versão, mas não explica tendência, recycles, pressão ou relação com erros.
- **Objective:** criar um workspace por namespace/name/version com navegação segmentada entre visão geral, arquivos e logs, série curta e eventos relacionados.
- **Value:** operador diferencia regressão, saturação, cold start e falha isolada.
- **Constraints:** série bounded e reset-aware; identidade versionada obrigatória.

## Traceability

- **Prototype:** `OBS-02 Worker detail`, `OBS-04 Request trace` e `OBS-06 Worker workspace · Logs` no Paper.
- **Business rules:** default e versão explícita nunca são misturadas; janela e restart aparecem na UI.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-worker/src/metrics.rs` | edit | Amostras/agregados estritamente necessários |
| `crates/edger-orchestrator/src/observability.rs` | edit | Série bounded por identidade |
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Endpoint de detalhe/série |
| `workers/core/cpanel/index.js` | edit | View detalhe e drill-down |
| `workers/core/cpanel/components/ui/tabs.js` | reuse | Navegação segmentada acessível |
| `crates/edger-orchestrator/tests/observability_api.rs` | edit | Janela/reset/identidade |

## Detail

### AS-IS

- Snapshot contém processos, requests, p95, wait, queue/reject/timeout e recycle counters, sem série servidor.

### TO-BE

- Cabeçalho mostra identidade, status/default, uptime e freshness.
- Segmented control alterna `Overview`, `Files` e `Logs`; cada modo possui pathname próprio e mantém estado após refresh/back/forward.
- `Overview` concentra métricas/processos, `Files` gerencia o conteúdo versionado e `Logs` abre erros/eventos da mesma identidade sem exigir uma stack externa.
- Charts: req/min, p95, active/idle/queued e errors/outcomes numa janela curta configurada.
- Blocos: capacity/processes, recycle reasons, queue pressure, latest errors e correlated events.
- Clique em request ID ou trace ID abre OBS-04 com timeline de eventos allowlisted.

### Scope

- **In:** série curta em memória, endpoint e UI; comparação básica entre versões do mesmo app.
- **Out:** long-term analytics, percentis distribuídos e flamegraphs.

### Acceptance criteria

- [x] Série preserva identidade e indica gaps/restart.
- [x] Worker cold, active, terminating e disabled têm estados próprios.
- [x] Charts não misturam janelas distintas sem aviso.
- [x] Detalhe de evento é filtrável por request ID/trace ID e usa envelope sanitizado.
- [x] Navegação segmentada é responsiva, acessível e restaura o modo selecionado após refresh.
- [x] `Settings` e `Deployments` não aparecem como abas vazias ou redundantes.

### Dependencies

- 21.03; contrato de eventos e identidade estabilizado.

## Tasks

- [x] Alinhar OBS-02/OBS-04/OBS-06 com o layout corrente.
- [x] Definir janela/buckets e orçamento bounded.
- [x] Implementar endpoint e testes de série.
- [x] Implementar detalhe, charts e drill-down.
- [x] Consolidar o workspace segmentado e validar history/back/forward/refresh.
- [x] Validar cenários reais e gates.

## Status

completed (2026-07-12) — endpoint root-only de série por identidade, workspace Files/Observability/Logs e tráfego real verificados.

## Verification

```bash
cargo test -p edger-orchestrator --test observability_api
planning/edger/scripts/cpanel-scenario.sh setup
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check
```
