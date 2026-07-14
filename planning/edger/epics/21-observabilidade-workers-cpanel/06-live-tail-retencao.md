# Story 21.06: Live tail e retenção explícita

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** polling perde eventos entre janelas e não oferece acompanhamento durante incidentes.
- **Objective:** adicionar atualização incremental opt-in com backpressure e limites claros.
- **Value:** operador acompanha uma reprodução sem recarregar ou consultar repetidamente.
- **Constraints:** SSE não pode bloquear runtime; desconexão e cliente lento devem ser seguros; store bounded continua autoridade.

## Traceability

- **Prototype:** estado `Live` de `OBS-03 Logs explorer` no Paper.
- **Business rules:** live tail inicia pausado ou explicitamente habilitado; UI indica dropped events e permite retomar do cursor.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Endpoint SSE root-only |
| `crates/edger-orchestrator/src/observability.rs` | edit | Subscription/cursor/backpressure |
| `workers/core/cpanel/index.js` | edit | Live/pause/resume e dropped state |
| `crates/edger-orchestrator/tests/observability_sse.rs` | create | Disconnect, cursor e slow client |
| `planning/edger/docs/compat-matrix.md` | edit | Limites e retenção |

## Detail

### AS-IS

- Gateway possui SSE próprio; workers usam polling/snapshots e stdout tracing.

### TO-BE

- SSE de eventos unificados aceita os mesmos filtros e `Last-Event-ID`/cursor.
- Cliente pode pausar renderização sem parar retenção bounded; badge informa novos/dropped.
- Reconnect busca backlog disponível e declara gap quando cursor expirou.

### Scope

- **In:** SSE local root-only, cursor, backpressure, pause/resume e docs de retenção.
- **Out:** websocket, cluster fanout, persistência, replay ilimitado e OTLP exporter (21.08).

### Acceptance criteria

- [x] Cliente lento/desconectado não bloqueia dispatch nem cresce memória sem limite.
- [x] Reconnect não duplica eventos e declara gaps irrecuperáveis.
- [x] Filtros SSE correspondem à API paginada.
- [x] UI permite pause/resume e mostra novos/dropped.

### Dependencies

- 21.04, 21.05 e 21.07.

## Tasks

- [x] Definir contrato SSE/cursor compatível com store.
- [x] Escrever testes de autorização, resume, filtros e gap de retenção.
- [x] Implementar endpoint e subscriptions bounded.
- [x] Implementar estado Live no explorador.
- [x] Medir, documentar e rodar gates.

## Verification

```bash
cargo test -p edger-orchestrator --test observability_sse
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

completed (2026-07-11) — SSE root-only usa o store como autoridade, broadcast bounded apenas para wake-up, cursor retomável, gap explícito e UI pausada por padrão com pause/resume.
