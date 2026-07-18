# Story 14.03: Deploy drag-and-drop no cPanel

**Origin:** `planning/edger/epics/14-deploy-apps/00-overview.md`

## Context

- **Problema:** com install API e rescan prontos, o desenvolvedor ainda precisa de `curl`; a promessa do produto é soltar um app na tela e ele estar no ar.
- **Objetivo:** deploy drag-and-drop de zip (e pasta via `webkitdirectory`/`DataTransferItem` empacotada em zip no cliente), preview do manifest inferido e confirmação → app no ar com URL clicável.
- **Decisão de UX (revisão 2026-07-02):** o deploy vive **dentro da view Workers** (deploy produz workers — mesma área), com um único botão **"Deploy app"** que abre uma modal (`Dialog` shadcn) de upload. Não há item "Deploy" na sidebar. **Sem botão Rescan separado:** o reconcile disco↔índice foi dobrado no **Refresh** — cada Refresh reconcilia a pasta de workers (indexa apps copiados por fora do install, remove apagados) e recarrega; falha de rescan (ex.: manifest quebrado no disco) é ignorada para não travar a leitura do índice. A badge "root" do header foi removida (redundante com o rodapé da sidebar que já mostra principal/role).
- **Valor:** paridade com o file manager DnD do Buntime, com UX de mini Vercel.
- **Restrições:** stack React + shadcn `components/ui/`; zip no cliente via `fflate`; limite dedicado de 64 MiB comunicado na UI.

## Traceability

- `workers/core/cpanel/` (Epic 12 + redesign 2026-07-02; catálogo shadcn em `components/ui/`)
- Buntime: file manager DnD do cPanel
- Stories 14.01/14.02 (APIs consumidas)

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/core/cpanel/index.js` | edit | View Deploy: dropzone, preview, chamada install, resultado com URL |
| `workers/core/cpanel/index.html` | edit | Import CDN de zip client-side (fflate) no importmap se necessário |
| `planning/edger/status/evidence/` | edit | Evidência Browser do fluxo DnD |

## Detail

### AS-IS

- cPanel tem Overview/Workers/Modules/Gateway/Keys; nenhum caminho de escrita de apps.

### TO-BE

- Header da tabela de Workers só com "Deploy app" (abre modal). Sem item "Deploy" na sidebar e sem botão Rescan separado.
- Modal (`Dialog` shadcn) só com o fluxo de upload: dropzone (zip/pasta), preview, install, Close.
- Refresh (topo) reconcilia a pasta de workers e recarrega — um botão para "sincronizar com a realidade do disco".
- Preview antes de confirmar: nome, versão, kind inferido, visibilidade, arquivos principais; erros de validação legíveis (traduzidos dos códigos `DEPLOY_*`).
- O preview aceita manifesto opcional e exibe os mesmos defaults do backend: nome do ZIP, versão `latest`, autodiscovery de `index.*` e kind inferido.
- Após confirmar: chamada `POST /api/admin/workers/install`; sucesso mostra URL clicável (`/<name>` ou `/@ns/<name>`) e "Deploy another"; a tabela de Workers por trás é atualizada. Rodapé da modal só com "Close" (o Rescan vive no header da listagem, não na modal).

### Scope

- **In:** dropzone, preview, install, feedback de sucesso/erro, botão rescan, evidência Browser.
- **Out:** editor de arquivos, diff de versões, upload acima do cap, deploy remoto.

### Acceptance criteria

- [x] Drop de zip válido mostra preview correto (nome/versão/kind) antes de qualquer escrita.
- [x] Confirmar instala e mostra URL; abrir a URL serve o app (validado no Browser).
- [x] Zip inválido/zip-slip mostra erro legível sem sujar o root.
- [x] Drop de pasta funciona em Chrome (webkitdirectory) empacotando zip no cliente. (input `webkitdirectory` + traversal de `webkitGetAsEntry` no drop; ambos convergem em `stageFileMap` → `zipSync`, o mesmo pipeline exercitado na validação com zip gerado client-side)
- [x] Reconcile disco↔índice acessível pela UI. (revisão final: dobrado no Refresh — cada Refresh reconcilia a pasta de workers e recarrega; sem botão Rescan separado)
- [x] Console/network do Browser sem erros no fluxo feliz.

### Dependencies

- Stories 14.01, 14.02; Epic 12 (cPanel)

## Test-first plan

- **Behavior:** validação Browser (preview screenshots + fluxo real) sobre APIs já cobertas por E2E Rust; sem lógica de negócio nova no cliente além de empacotar/preview.
- **Level:** evidência Browser + testes Rust existentes das APIs.
- **Avoid:** duplicar validação de manifest no cliente (preview usa resposta de dry-run/erros da API quando possível).

## Tasks

### Fase 1 — Dropzone + preview
- [x] View Deploy com dropzone (drag events + input file fallback).
- [x] Leitura do zip no cliente para preview (nome/versão/kind) sem escrever nada.

### Fase 2 — Install + feedback
- [x] Chamada install com tratamento de `401/403/400/409/413`.
- [x] Tela de sucesso com URL clicável + link para Workers.

### Fase 3 — Pasta + rescan + evidência
- [x] Empacotar pasta em zip no cliente (fflate CDN).
- [x] Botão Rescan (dry-run → confirmar apply).
- [x] Evidência Browser em `planning/edger/status/evidence/`.

## Verification

```bash
cargo test -p edger-orchestrator --test deploy_install
# Browser: drop de zip válido, inválido e pasta; screenshots como evidência
```

## Status

**completed** (2026-07-02) — deploy consolidado dentro da view **Workers**:
botão "Deploy app" no header da tabela abre uma **modal** (`Dialog` shadcn,
`#deploy-app-dialog`) com dropzone (drag/drop de zip, drop de pasta via
traversal `webkitGetAsEntry`, inputs zip/pasta),
preview client-side do pacote (nome, versão, kind inferido, entrypoint,
arquivos, visibility via `fflate.unzipSync`), install com feedback "is live"
(URL clicável + badges kind/visibility + dica de auth + atalho Workers),
erros legíveis e rodapé só com Close; a tabela de Workers por trás é atualizada
ao instalar. **Rescan dobrado no Refresh** (revisão final): um único botão
Refresh reconcilia a pasta de workers e recarrega — sem botão Rescan separado.
Badge "root" do header removida. Não há item "Deploy" na sidebar. Validado no
preview builtin: deploy por modal → app live; worker copiado no disco aparece
após Refresh; worker apagado do disco some após Refresh (404). Console/network
sem erros. Evidência: `status/evidence/deploy-vertical-slice-2026-07-02.txt`.

Revisão 2026-07-18: a modal deixou de exigir `manifest.yaml`, passou a enviar
o nome do arquivo como hint estável e corrigiu a indicação do limite para
64 MiB.
