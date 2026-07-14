# Story 14.02: Rescan de workers — reconciliar disco ↔ índice em runtime

**Origin:** `planning/edger/epics/14-deploy-apps/00-overview.md`

## Context

- **Problema:** workers copiados manualmente (ou escritos pelo MCP do Epic 13) só aparecem após restart; o índice não tem caminho de re-derivação do disco.
- **Objetivo:** `POST /api/admin/workers/rescan` com dry-run (diff disco↔índice) e apply, seguindo o padrão do reconcile de extensões (Epic 10.02).
- **Valor:** deploy por qualquer via (cp, git pull, MCP, rsync) vira operação de runtime; o disco é a fonte de verdade.
- **Restrições:** não remove workers com tráfego sem sinalizar; não promete hot reload de extensões Rust.

## Traceability

- `crates/edger-orchestrator/src/registry.rs` (reconcile de extensões — contrato dry-run/apply)
- `crates/edger-orchestrator/src/manifest_loader.rs` (`load_manifests_from_dirs` — re-scan)
- `crates/edger-mcp/` (Epic 13 escreve workers no disco; rescan fecha o ciclo authoring→online)

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/deploy.rs` | edit | Diff disco↔índice + apply reaproveitando validação da 14.01 |
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Rota `POST /api/admin/workers/rescan` (`workers:install`) |
| `crates/edger-orchestrator/src/manifest_index_stub.rs` | edit | Suporte a remoção/substituição de entrada por `name@version` se necessário |
| `crates/edger-orchestrator/tests/deploy_install.rs` | edit | Casos de rescan (added/removed/changed; dry-run vs apply) |

## Detail

### AS-IS

- Índice construído uma vez no boot; `enable/disable` é overlay, não re-scan.
- Reconcile dry-run/apply existe apenas para extensões.

### TO-BE

- `POST /api/admin/workers/rescan` com `{ "dryRun": true|false }`.
- Dry-run responde `{ added: [...], removed: [...], unchanged: n }` comparando `load_manifests_from_dirs(worker_roots)` com o índice atual.
- Apply insere workers novos e remove do índice os que sumiram do disco (workers em execução no pool terminam pelo ciclo normal TTL/ephemeral).
- Enable/disable persistido é respeitado (overlay reaplicado após rescan).

### Scope

- **In:** diff, dry-run, apply, preservação de overlay enable/disable, testes.
- **Out:** watch de filesystem (inotify) automático; reconcile de extensões (já existe); UI (14.03).

### Acceptance criteria

- [x] Worker copiado manualmente para o root aparece no dry-run como `added` e responde após apply, sem restart.
- [x] Worker removido do disco aparece como `removed` e sai do índice após apply (rota volta 401/404 conforme resolução).
- [x] Dry-run não muda estado (chamadas repetidas idênticas).
- [x] Worker `disabled` continua `disabled` após rescan.
- [x] Permissão `workers:install` exigida; `403` sem ela.

### Dependencies

- Story 14.01 (validação e indexação incremental compartilhadas)

## Test-first plan

- **Behavior:** E2E na pipeline HTTP com root em tempdir: copiar worker → dry-run → apply → fetch.
- **Level:** `crates/edger-orchestrator/tests/deploy_install.rs` + workspace gate.
- **Avoid:** inspecionar structs internas do índice; asserções devem ser observáveis via API/fetch.

## Tasks

### Fase 1 — Diff
- [x] Calcular diff disco↔índice por `name@version` reutilizando `load_manifests_from_dirs`.
- [x] Resposta dry-run `{ added, removed, unchanged }`.

### Fase 2 — Apply
- [x] Apply insere/remove no índice preservando overlay enable/disable.
- [x] Rota + permissão + testes E2E (added, removed, idempotência do dry-run).

### Fase 3 — Fechamento
- [x] Documentar rescan junto do install nos docs de operação.
- [x] Gate workspace completo.

## Verification

```bash
cargo test -p edger-orchestrator --test deploy_install -- rescan
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed** (2026-07-02) — `POST /api/admin/workers/rescan` com dry-run
default e apply (`{"dryRun":false}`), diff disco↔índice por `name@version`
via `scan_worker_manifests` (roots gravados no `ManifestIndex` pelo boot),
`remove_worker` limpa entries/host routes/plugins/shell. Provado live:
worker removido do disco saiu do índice sem restart. Evidência:
`status/evidence/deploy-vertical-slice-2026-07-02.txt`.
