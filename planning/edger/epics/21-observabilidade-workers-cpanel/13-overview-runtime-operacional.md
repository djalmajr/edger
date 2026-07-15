# Story 21.13: Overview operacional compreensivo

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problema:** o Overview atual é um inventário com contadores acumulados e
  não responde rapidamente se a instância está saudável, o que exige atenção
  ou quais workers concentram carga e falhas.
- **Objetivo:** transformar `/cpanel/` em uma visão operacional acionável,
  mantendo `/cpanel/observability` como superfície de investigação temporal.
- **Restrições:** usar somente métricas, séries e eventos bounded existentes;
  declarar janela e freshness; não apresentar snapshot como histórico.

## Traceability

- **Tela:** `/cpanel/` — Overview.
- **Contratos:** `/metrics/stats`, `/api/admin/observability/series` e
  `/api/admin/observability/events`.
- **Decisões:** observabilidade local-first; routing, processo e health são
  dimensões independentes; health sem tráfego é `Unobserved`.

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/core/cpanel/src/lib/overview.ts` | create | Cálculos operacionais puros |
| `workers/core/cpanel/src/lib/overview.test.ts` | create | Provar status, atenção, capacidade e ranking |
| `workers/core/cpanel/src/lib/api.ts` | edit | Completar DTO já exposto por `/metrics/stats` |
| `workers/core/cpanel/src/main.tsx` | edit | Compor o novo Overview e suas ações |
| `planning/edger/scripts/cpanel-ui-gate.sh` | edit | Proteger os contratos principais da tela |
| `planning/edger/status/evidence/` | edit | Registrar gates e validação Browser |

## Detail

### AS-IS

- Cards mostram inventário e request total acumulado desde o restart.
- `Needs attention` agrega somente versões desabilitadas e error summary.
- Pool não mostra filas, rejeições, timeouts, distribuição de health, ranking
  ou atividade recente.

### TO-BE

- Health geral na headline, polling de cinco segundos e refresh manual.
- Headline com apps, versões, health, requests/erros em cinco minutos e
  processos atuais.
- Atenção priorizada por worker/versão com destino para Workers ou logs.
- Capacidade do runtime, distribuição de health, workers mais ativos e eventos
  operacionais recentes em cards compactos e responsivos.

### Scope

- **In:** composição read-only de contratos existentes e navegação contextual.
- **Out:** histórico persistente, alert manager, SLO distribuído, CPU/RSS da
  instância e inventar uptime/version do servidor sem contrato próprio.

### Approach

- Cálculos ficam em módulo puro para não acoplar regras à renderização.
- Séries e eventos usam polling de cinco segundos e retenção já bounded.
- `Critical` exige worker `Failing`; `Degraded` cobre health degradado, erro
  recente, rejeição/timeout, fila ou versão desabilitada; caso contrário,
  `Healthy`.

### Risks

- **Contadores cumulativos parecerem janela:** headline usa a série de cinco
  minutos; cumulativos permanecem apenas em detalhes explicitamente rotulados.
- **Duplicar Observability:** Overview resume e navega; séries detalhadas e
  exploração continuam na rota dedicada.

## Acceptance criteria

- [x] O Overview declara health geral e janela de cinco minutos sem duplicar
  metadados do polling automático.
- [x] Requests, erros e p95 da headline vêm da série bounded, não de acumulados.
- [x] Atenção, capacidade, health, ranking e atividade usam contratos existentes.
- [x] Ações de atenção e atividade navegam para o contexto operacional correto.
- [x] Estado `Unobserved` permanece distinto de `Degraded` e `Failing`.

## Test-first plan

- **Red:** testes do resumo operacional falham antes de existir o módulo.
- **Green:** implementar o mínimo para status, distribuição, capacidade,
  atenção e ranking.
- **Refactor:** reutilizar o resumo na UI e manter queries separadas da regra.
- **Nível:** unitário para cálculos; Browser para composição e navegação.
- **Evitar:** snapshots de markup e testes que apenas procuram texto estático.

## Tasks

- [x] Provar cálculos operacionais com factories e casos Healthy/Degraded/Critical.
- [x] Completar tipos do snapshot sem alterar o contrato Rust.
- [x] Implementar headlines, polling e refresh manual.
- [x] Implementar atenção, capacidade, health distribution, ranking e atividade.
- [x] Atualizar gate, evidência e validar no Browser com dados reais.

## Verification

```bash
cd workers/core/cpanel && bun test && bun run build
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed** (2026-07-15) — cálculos unitários, build, gates Rust/UI e fluxo
Browser validados; evidência em
`planning/edger/status/evidence/overview-runtime-operacional-2026-07-15.md`.
