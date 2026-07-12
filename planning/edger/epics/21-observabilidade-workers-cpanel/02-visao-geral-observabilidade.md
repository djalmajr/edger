# Story 21.02: Visão geral de observabilidade

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** sinais existentes aparecem dispersos entre Overview, Workers e dialogs.
- **Objective:** criar a rota/view Observability com visão operacional baseada apenas em contratos atuais.
- **Value:** diagnóstico inicial de throughput, latência, pool, filas e erros em uma tela.
- **Constraints:** snapshots não podem ser apresentados como histórico persistente.

## Traceability

- **Prototype:** `OBS-01 Observability overview` no Paper.
- **Business rules:** toda métrica exibe janela/fonte/freshness; reset de runtime é explícito.

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/cpanel/index.js` | edit | Nova view e navegação Observability |
| `workers/cpanel/components/ui/chart.js` | reuse | Sparklines/séries cliente bounded |
| `planning/edger/scripts/cpanel-ui-gate.sh` | edit | Contrato visual e de fontes |
| `planning/edger/status/evidence/` | create | Browser proof com tráfego real |

## Detail

### AS-IS

- `/metrics/stats` já fornece pool e workers; cPanel mantém apenas uma janela cliente curta.

### TO-BE

- Cards: request rate, p95, active/idle, queue pressure e errors recent.
- Lista `Workers requiring attention` ordenada por erro, timeout/reject e latência.
- Mini séries vêm do store bounded do processo e declaram `live session`, janela e reset parcial.
- Atalhos abrem OBS-02 e OBS-03 filtrados.

### Scope

- **In:** composição de APIs existentes, polling único, empty/loading/stale/error states.
- **Out:** persistência de longo prazo, alert manager e agregação entre réplicas.

### Acceptance criteria

- [x] Nenhuma métrica é inventada ou rotulada como histórica.
- [x] Tráfego real atualiza cards e ranking por polling bounded sem recarregar.
- [x] Falha parcial de uma API preserva último snapshot confiável e indica staleness.
- [x] Layout responsivo não cria scroll horizontal e o ranking é limitado a dez workers.

### Dependencies

- 21.01 para navegação de atenção consistente.

## Tasks

- [x] Alinhar OBS-01 no Paper com o layout corrente.
- [x] Implementar polling único de metrics e série com last-good/stale.
- [x] Implementar cards, ranking e janela móvel de 60 segundos.
- [x] Validar tráfego, erro, pressão e ausência de dados.
- [x] Registrar evidência e gates.

## Status

completed (2026-07-12) — rota `/cpanel/observability` validada no Browser com tráfego real, séries curtas, ranking acionável e layout responsivo.

## Verification

```bash
planning/edger/scripts/cpanel-scenario.sh setup
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
