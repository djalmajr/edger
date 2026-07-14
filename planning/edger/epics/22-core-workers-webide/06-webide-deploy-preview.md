# Story 22.06: Deploy explícito, preview e histórico

**Origin:** `planning/edger/epics/22-core-workers-webide/00-overview.md`

## Context

Empacotar um snapshot determinístico, usar o install pipeline existente e só
trocar o preview quando o deploy terminar com sucesso.

## Files

- `workers/core/webide/src/app.js`
- `workers/core/webide/src/index.html`
- `workers/core/webide/src/styles.css`

## Detail

O deploy usa o endpoint ZIP existente. O iframe aponta somente para a versão
confirmada e não recebe a sessão administrativa da WebIDE.

## Tasks

- [x] Validar manifesto e criar ZIP determinístico.
- [x] Exibir estágios e histórico local de deployments.
- [x] Preservar o preview anterior quando o pipeline falha.
- [x] Isolar o iframe e oferecer abertura externa.
- [x] Implantar os três kinds iniciais no Browser.

## Acceptance criteria

- [x] ZIP ordena arquivos e fixa metadados para ser determinístico.
- [x] UI mostra validação, empacotamento, upload, release/health, ativação e fim.
- [x] Preview usa pathname versionado e conserva versão anterior em falha.
- [x] Iframe não recebe credenciais e não usa `allow-same-origin`.
- [x] Histórico local e links para logs/eventos estão disponíveis.
- [x] FetchHandler, RoutesTable e StaticSpa foram implantados no Browser.

## Verification

- Comparação byte a byte do ZIP para o mesmo snapshot.
- E2E de sucesso e falha no Browser.

## Status

completed (2026-07-13).
