# Story 14.07: Control plane serverless para o Studio

**Origin:** `planning/edger/epics/14-deploy-apps/00-overview.md`

## Context

- **Problema:** o lowcode-studio precisa autorar, testar, publicar, promover e remover workers sem expor rascunhos no data plane nem reiniciar o EdgeR.
- **Objetivo:** completar o ciclo serverless com draft interno mutável, releases públicas imutáveis, promoção durável, invocação autenticada e paridade MCP segura.
- **Valor:** o Studio opera o runtime no modelo Deno Deploy sem acesso direto ao PVC ou à chave root em argumentos de tool.
- **Restrições:** preservar isolamento por processo, não permitir mutação de release pública, não encaminhar credenciais do control plane ao worker e manter o alvo MCP ligado à configuração do servidor.

## Traceability

- `planning/edger/epics/14-deploy-apps/01-install-api.md`
- `planning/edger/epics/14-deploy-apps/04-versoes-rollback.md`
- `planning/edger/epics/14-deploy-apps/06-arquivos-seguros-cpanel.md`
- `crates/edger-orchestrator/src/admin_api.rs`
- `crates/edger-mcp/src/contracts.rs`

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/manifest.rs` | edit | Declarar `visibility: public\|internal` com default público |
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Delete, force-draft, promote, invoke e autorização |
| `crates/edger-orchestrator/src/deploy.rs` | edit | Swap atômico, remoção e ciclo de deploy |
| `crates/edger-orchestrator/src/manifest_index_stub.rs` | edit | Default explícito e resolução pública separada do draft |
| `crates/edger-orchestrator/src/manifest_loader.rs` | edit | Persistir/restaurar promoção no startup e rescan |
| `crates/edger-worker/src/pool.rs` | edit | Reciclar processos por worker/versão |
| `crates/edger-isolation/src/multiproc_harness.mjs` | edit | `Allow` correto em method-map 405 |
| `crates/edger-mcp/src/control_plane.rs` | create | Tools HTTP do control plane com alvo fixo no contexto |
| `crates/edger-mcp/src/contracts.rs` | edit | Contratos MCP sem URL/credencial em argumentos |

## Detail

### AS-IS

- Install recusava toda versão duplicada; drafts não tinham canal de live reload.
- O default era inferido pela maior semver e se perdia em restart.
- Workers não possuíam visibilidade interna nem invocação autenticada dedicada.
- MCP cobria autoria local, não o ciclo completo do control plane.

### TO-BE

- `visibility: internal` nunca é servido pelo data plane aberto; somente o endpoint admin autenticado o invoca.
- `force=true` substitui apenas uma versão interna existente por staging + swap e recicla seu pool; releases públicas continuam imutáveis.
- Force é compare-and-swap: todo install grava `.edger-revision` dentro do diretório da versão (viaja no swap, sobrevive a restart/rescan) e devolve `revision`; `force` exige `x-edger-expected-revision` e responde `409 DEPLOY_REVISION_STALE` para revisão defasada — dois autosaves sobrepostos nunca publicam o mais velho por último. Upload de arquivos também avança a revisão.
- Um `WorkerMutationSlot` (RAII, chave `root|name@version` canonicalizada) reserva o alvo pela transação INTEIRA — install (check→swap→release→health→commit/rollback), delete (todas as versões em ordem estável) e upload de files (antes de qualquer mutação, inclusive `create_dir_all`) — e o concorrente recebe `409 DEPLOY_IN_PROGRESS`; sem isso o rollback de um force ressuscitaria um delete ou clobberaria um upload.
- Promote aceita somente versão pública, persiste o ponteiro por escrita temp + rename e o recarrega em startup/rescan.
- Delete remove versão específica ou worker inteiro do disco, índice e pool.
- MCP instala, lista, habilita, desabilita, remove, promove, invoca e consulta eventos; URL/chave vêm exclusivamente do `McpContext`.

### Scope

- **In:** contratos admin/MCP, visibilidade, live reload interno, default persistido, remoção, invocação, reciclagem e regressões.
- **Out:** canary percentual, edição de visibilidade em versão existente, GC automático de releases e transporte MCP de stream infinito.

### Acceptance criteria

- [x] Delete por versão e por worker remove disco, índice e processos; ausente retorna 404.
- [x] Force substitui somente draft interno, troca arquivos atomicamente e recicla processo aquecido.
- [x] Release pública não aceita force nem upload de arquivos; nova mudança pública exige nova versão + promote.
- [x] Promote recusa draft interno, mantém versões pinadas e sobrevive a reconstrução do índice.
- [x] Force exige revisão esperada (CAS): sem header responde `DEPLOY_REVISION_REQUIRED`, revisão defasada responde `DEPLOY_REVISION_STALE`, e a revisão avança a cada substituição e upload de arquivos.
- [x] Mutações concorrentes do mesmo `name@version` (install/delete/files) são serializadas pelo slot de mutação: exatamente um writer vence; os demais recebem `DEPLOY_IN_PROGRESS`, mesmo durante release/health, e o rollback restaura o estado pré-force.
- [x] Data plane devolve 404 para worker interno e ignora marcador interno forjado.
- [x] Invoke autenticado preserva método, headers, body, streaming e query da aplicação; a versão-alvo usa header de controle separado.
- [x] Respostas de install/list expõem `visibility`.
- [x] Tools MCP espelham o control plane sem aceitar URL ou chave em argumentos; HTTP externo exige HTTPS.
- [x] Delete MCP exige `version` ou `allVersions: true` explícito.
- [x] Method-map do harness mantém função direta e devolve 405 com `Allow`.

### Dependencies

- Story 14.01
- Story 14.04
- Story 14.06

## Test-first plan

- **Behavior:** provar draft interno → live reload → release pública → promote/rollback persistido → delete.
- **First failing tests:** endpoints ausentes, versão duplicada em force, draft roteável no data plane e promote perdido após reload.
- **Level:** integração Axum/pool em `deploy_install.rs`, UDS real para o harness e contrato HTTP MCP com servidor local.
- **Avoid:** testes de texto-fonte, aliases temporários e mocks que não atravessam o pipeline real de dispatch.

## Tasks

### Fase 1 — Segurança e ciclo de deploy
- [x] Adicionar visibilidade normalizada e bloquear `internal` no data plane.
- [x] Implementar delete e force restrito a draft com reciclagem de pool.
- [x] Tornar releases públicas imutáveis também no upload de arquivos.

### Fase 2 — Promoção e invocação
- [x] Persistir default público atomicamente e restaurá-lo no startup/rescan.
- [x] Implementar invoke autenticado com versão em `x-edger-worker-version` e query da aplicação intacta.
- [x] Gravar/comparar `.edger-revision` (CAS do force com `x-edger-expected-revision`) e reservar `name@version` por `WorkerMutationSlot` até commit/rollback, compartilhado com delete e files.
- [x] Expor visibilidade nos payloads administrativos.

### Fase 3 — MCP e compatibilidade
- [x] Adicionar tools do control plane e filtro de eventos por worker.
- [x] Fixar URL/chave no contexto MCP e exigir HTTPS fora de loopback.
- [x] Exigir seletor destrutivo explícito na tool de delete.
- [x] Cobrir method-map + 405/Allow no processo Deno persistente.

## Verification

```bash
cargo test -p edger-orchestrator
cargo test -p edger-isolation --features multiproc --test uds_roundtrip routes_method_map_dispatches_and_reports_allow_on_405 -- --exact
cargo test -p edger-mcp --test protocol
planning/edger/scripts/run-gates.sh
```

## Status

**completed** (2026-08-24) — ciclo serverless do Studio implementado com draft interno mutável, release pública imutável, promoção durável, invocação autenticada, remoção completa, reciclagem de pool e tools MCP com alvo seguro.
