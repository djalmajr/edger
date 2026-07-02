# Story 14.05: Transparência pós-deploy

**Origin:** `planning/edger/epics/14-deploy-apps/00-overview.md`

## Context

- **Problema:** deploy "cego" força o desenvolvedor a adivinhar a URL e caçar erros; transparência é metade da promessa do produto.
- **Objetivo:** resposta de install/deploy sempre inclui URL, kind inferido e visibilidade; falhas do primeiro request ficam acessíveis (logs operacionais por worker) na UI e na API.
- **Valor:** o desenvolvedor vê imediatamente onde o app está e por que falhou, sem SSH/log-diving.
- **Restrições:** reutilizar operational logs (08.29) e métricas existentes; sem APM novo.

## Traceability

- `edger-orchestrator/src/operational_log.rs` (08.29)
- Stories 14.01/14.03 (resposta de install e UI)
- `/metrics` + gateway log stats (08.19/08.20)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/deploy.rs` | edit | Enriquecer resposta de install (URL, kind, visibility, dicas de auth) |
| `edger-orchestrator/src/admin_api.rs` | edit | Endpoint de logs operacionais filtrado por worker, se ainda não existir |
| `workers/cpanel/index.js` | edit | Tela pós-deploy: URL, kind, últimos erros do worker |
| `edger-orchestrator/tests/deploy_install.rs` | edit | Asserções sobre payload de resposta e logs após primeiro erro |

## Detail

### AS-IS

- Operational error logs existem (08.29) mas não são apresentados no fluxo de deploy.
- Install (14.01) responde metadados mínimos.

### TO-BE

- Resposta de install: `{ name, version, url, kind, visibility, authRequired }`.
- cPanel pós-deploy mostra URL clicável + "últimos erros deste worker" (vazio no caminho feliz).
- Erro no primeiro request do worker aparece no painel sem procurar em stdout.

### Scope

- **In:** payload enriquecido, logs por worker no fluxo de deploy, UI pós-deploy.
- **Out:** tracing distribuído, alertas, retenção configurável de logs.

### Acceptance criteria

- [x] Resposta de install contém URL, kind, visibility e `authRequired` coerente com visibility. (`install_response_reports_auth_required_from_visibility`)
- [x] Worker que lança erro no primeiro request tem o erro visível via admin API por worker (`/api/admin/workers/{name}/errors` + `error-summary`), mensagem ANSI-stripped. (`worker_first_request_error_is_visible_via_admin_api`)
- [x] Estado (erros) exibido na **listagem de Workers** (badge de erro por worker + dialog de detalhes), não na modal — decisão de UX do operador: a modal é exclusiva do ato de deploy. cPanel pós-deploy mostra URL/kind/visibility e aponta para a lista.

### Dependencies

- Stories 14.01, 14.03; 08.29 (operational logs)

## Test-first plan

- **Behavior:** E2E install → request com erro → admin API mostra o erro do worker.
- **Level:** `edger-orchestrator/tests/deploy_install.rs`.
- **Avoid:** parsear logs de stdout; usar o contrato da API.

## Tasks

### Fase 1 — Payload
- [x] Resposta de install enriquecida com `authRequired` (URL/kind/visibility já existiam) + testes.

### Fase 2 — Logs por worker
- [x] `WorkerErrorLog` (ring buffer por worker em `ServerState`) alimentado pelo pipeline no erro de dispatch; rotas `GET /api/admin/workers/{name}/errors` e `/error-summary` (workers:read).

### Fase 3 — UI
- [x] Erros na listagem de Workers (badge destrutivo `N errors` + dialog com status/code/requestId/mensagem); modal de deploy exclusiva do ato de deploy (Close outlined). Evidência Browser capturada.

## Verification

```bash
cargo test -p edger-orchestrator --test deploy_install -- transparency
cargo test --workspace
```

## Status

**completed** (2026-07-02) — transparência pós-deploy entregue com decisão de UX
do operador: a **modal de deploy é exclusiva do ato de deploy** (upload +
confirmação + URL); qualquer estado de saúde/erro vive na **listagem de
Workers**. Backend: `WorkerErrorLog` (ring buffer por worker, cap 20, mensagem
ANSI-stripped) em `ServerState`, alimentado pelo pipeline no erro de dispatch;
`InstalledWorker.authRequired` derivado de visibility; rotas
`GET /api/admin/workers/{name}/errors` e `/error-summary` (workers:read). UI:
badge destrutivo `N errors` na coluna Status de cada worker + dialog com
detalhes (status/code/requestId/mensagem); a modal perdeu o health-check.
E2E `worker_first_request_error_is_visible_via_admin_api` (record + endpoint +
summary + ANSI strip) e `install_response_reports_auth_required_from_visibility`
(mutações provadas). Validado no preview: worker público que lança no handler →
badge "1 error" na listagem → dialog com o erro limpo. Evidência:
`status/evidence/deploy-vertical-slice-2026-07-02.txt`.
