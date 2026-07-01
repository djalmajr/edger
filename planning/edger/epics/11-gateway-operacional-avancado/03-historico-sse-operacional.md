# Story 11.03: Historico e SSE operacional

**Origin:** `planning/edger/epics/11-gateway-operacional-avancado/00-overview.md`

**Status:** completed

## Context

O gateway tem ring buffer local e endpoints read-only, mas operacao real precisa observar eventos recentes e receber stream local seguro. Essa story adiciona historico/SSE sem transformar isso em UI final.

**Depende de:** Story 11.01, Epic 09

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/src/lib.rs` | edit | Consolidar historico e evento operacional seguro |
| `edger-orchestrator/src/admin.rs` | edit | Expor endpoint SSE root-only se necessario |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Provar auth, filtro e evento |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar consumo local |

## Detail

### AS-IS

- `/api/admin/gateway/logs` e stats expõem estado recente.
- Gateway pode persistir historico com provider externo em teste.
- SSE ainda nao existe.

### TO-BE

- Eventos de gateway tem request ID, decision, status, duracao e campos redigidos.
- SSE local root-only permite acompanhar eventos sem polling.
- Historico persistente fica opcional e usa provider duravel quando configurado.

### Scope

- **In:** contrato de evento, SSE local, auth, filtros, redaction.
- **Out:** painel visual, retenção longa, streaming remoto.

### Critérios de aceite

- [x] SSE exige autenticacao root.
- [x] Evento nao contem Authorization, Cookie, body nem query sensivel.
- [x] Historico e stream compartilham o mesmo contrato de evento.
- [x] Teste prova pelo menos um evento emitido em fluxo real de gateway.

## Tasks

- [x] Definir contrato de evento seguro.
- [x] Implementar emissao de evento e buffering local.
- [x] Expor SSE root-only ou endpoint equivalente local.
- [x] Adicionar testes de auth, redaction e fluxo.

## Verification

```bash
cargo test -p edger-ext-gateway
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Closure

completed (2026-07-01) - `/api/admin/gateway/logs/stream` expõe eventos SSE
`gateway.decision` root-only usando o mesmo objeto JSON redigido de
`/api/admin/gateway/logs`. O endpoint aceita os mesmos filtros (`limit`,
`rateLimited`, `status`, `decision`) e retorna `text/event-stream`. O teste
`gateway_admin_logs_stream_is_root_only_and_emits_redacted_events` executa um
fluxo real do middleware gateway, completa a resposta, valida `401` sem root,
consome o evento com root e confirma que Authorization, Cookie, body e query
sensivel nao aparecem no payload.
