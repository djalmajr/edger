# Story 21.05: Explorador de logs e correlação

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** operador não consegue consultar eventos de worker; dialog atual mostra somente até 10 erros recentes por nome.
- **Objective:** entregar no próprio cPanel um explorador filtrável e detalhe seguro por evento/request/trace ID, sem depender de OTEL ou backend externo.
- **Value:** reduz terminal log-diving e conecta sintomas às execuções responsáveis.
- **Constraints:** não é full-text; erros recentes existentes são a primeira fonte da aba; console vem somente da captura bounded da 21.07; paginação cursor; filtros preservados na URL/estado da view.

## Traceability

- **Prototype:** `OBS-03 Logs explorer`, `OBS-04 Request trace` e `OBS-06 Worker workspace · Logs` no Paper.
- **Business rules:** filtros por namespace/name/version/processId; detalhe mostra apenas campos allowlisted; copiar request/trace ID é ação explícita.

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/cpanel/index.js` | edit | Views logs e request trace |
| `workers/cpanel/components/ui/table.js` | reuse | Tabela compacta e acessível |
| `workers/cpanel/components/ui/sheet.js` | reuse | Detalhe de evento/request |
| `edger-orchestrator/src/admin_api.rs` | edit | Ajustar filtros/cursor conforme uso real |
| `planning/edger/scripts/cpanel-ui-gate.sh` | edit | Contratos de filtros e estados |

## Detail

### AS-IS

- Dialog `Recent errors` não filtra por versão, tempo, outcome ou request ID.

### TO-BE

- O workspace do worker/versão ganha aba `Logs` canônica; ela abre no escopo da identidade atual e começa com os erros recentes já disponíveis.
- A navegação principal ganha `Observability`; o explorador global agrega workers e preserva filtros pré-aplicados ao sair do workspace.
- Rota canônica `/cpanel/observability/logs` serializa intervalo, worker, versão, processo, nível, source e correlação na query string.
- Toolbar: tempo, worker, versão, processo, source, level, outcome/status, request ID e trace ID.
- Tabela virtual/paginada: timestamp, source, worker@version, process, outcome, status, duration, request/trace ID.
- Sheet abre evento e timeline do request/trace; filtros podem ser copiados como link local da view.
- Empty, loading, stale, dropped-events e permission-denied são estados explícitos.

### Scope

- **In:** consulta, paginação, filtros, detalhe, copy request ID e deep link no cPanel.
- **Out:** edição/delete, full-text, download ilimitado e query language.

### Acceptance criteria

- [x] Filtros combinados retornam somente eventos esperados.
- [x] Paginação não duplica nem perde eventos sob novas inserções.
- [x] Detalhe e clipboard não expõem campos proibidos.
- [x] Centenas de eventos não travam a interface.
- [x] Refresh e compartilhamento do URL restauram rota, filtros e evento/request selecionado.
- [x] Toda a jornada funciona com endpoint OTLP ausente e sem Collector.
- [x] Depois de 21.07, a aba identifica `Runtime events`, `Worker console`, release e lifecycle sem mudar a rota ou exigir OTEL.

### Dependencies

- Erros recentes atuais permitem o primeiro slice da aba `Logs`; 21.03 adiciona eventos versionados/correlacionáveis e 21.07 adiciona console logs reais.

## Tasks

- [x] Consolidar OBS-03/OBS-04/OBS-06 no layout atual.
- [x] Implementar toolbar e estado serializável.
- [x] Implementar a aba `Logs` no workspace por worker/versão e a navegação global agregada.
- [x] Implementar tabela/cursor e diálogo de detalhe.
- [x] Adicionar ações de correlação/cópia.
- [x] Validar volume, filtros, acessibilidade e gates.

## Verification

```bash
cargo test -p edger-orchestrator --test observability_api
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check
```

## Status

completed (2026-07-11) — explorador global e por worker/versão, filtros serializados, paginação cursor, detalhe allowlisted e deep link do evento foram validados no Browser sem Collector/OTLP.
