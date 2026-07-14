# Story 14.01: Install API — upload de pacote com indexação sem restart

**Origin:** `planning/edger/epics/14-deploy-apps/00-overview.md`

## Context

- **Problema:** o único caminho de deploy é copiar diretório + reiniciar o binário; nenhuma API instala um worker.
- **Objetivo:** `POST /api/admin/workers/install` recebe um zip do app, valida, escreve atomicamente no worker root e indexa o worker em runtime.
- **Valor:** primeiro passo do "mini Vercel": um `curl` (ou o cPanel na 14.03) coloca um app no ar sem tocar em infra.
- **Restrições:** body cap global de 4 MiB (`MAX_BODY_BYTES`); sem build server-side; extração deny-by-default contra zip-slip; permissão `workers:install` (paridade Buntime).

## Traceability

- `crates/edger-orchestrator/src/admin_api.rs` (padrão de rotas/permissões)
- `crates/edger-orchestrator/src/manifest_loader.rs` (`load_worker_manifest` como validação canônica)
- `crates/edger-orchestrator/src/manifest_index_stub.rs` (`ManifestIndex::insert` em runtime via `Arc<RwLock>`)
- Buntime: `workers:install` + upload de pacote no cPanel

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/deploy.rs` | create | Extração segura de zip, validação, escrita atômica, indexação |
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Rota `POST /api/admin/workers/install` + permissão `workers:install` |
| `crates/edger-orchestrator/src/lib.rs` | edit | `OrchestratorState.worker_roots` (destino de install) + export do módulo |
| `crates/edger-orchestrator/src/bin/edger.rs` | edit | Propagar `RUNTIME_WORKER_DIRS` parseado para o state |
| `crates/edger-orchestrator/Cargo.toml` | edit | Dependência `zip` (extração) |
| `crates/edger-orchestrator/tests/deploy_install.rs` | create | E2E: install → worker responde sem restart; negativos de segurança |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar o fluxo de install local |

## Detail

### AS-IS

- `RUNTIME_WORKER_DIRS` é lido no boot; o índice nunca muda depois (exceto enable/disable overlay).
- Admin API não tem rota de install; `workers:install` não é usada.
- Nenhuma dependência de zip no workspace.

### TO-BE

- `POST /api/admin/workers/install` com `content-type: application/zip` e body = pacote do app.
- Pipeline: (1) checar permissão `workers:install`; (2) extrair para diretório temporário dentro do root; (3) validar com `load_worker_manifest` (mesma regra do boot: `manifest.yaml`/`package.json`/`index.*`); (4) checar colisão `name@version` no índice; (5) rename atômico para o destino final; (6) `ManifestIndex::insert`; (7) responder `{ worker, url, kind, visibility }`.
- Zip-slip: cada entry canonicalizada precisa continuar sob o diretório de extração; entries absolutas ou com `..` são rejeitadas com erro tipado.
- Colisão: `409` com código `COLLISION` (mesma semântica do índice); instalar nova versão é permitido.
- Destino: primeiro root de `worker_roots` (v1); nome do diretório derivado do nome do worker (namespace vira subpath seguro).

### Scope

- **In:** rota install, extração segura, validação, escrita atômica, indexação em runtime, testes E2E + negativos.
- **Out:** multipart/pasta (14.03 empacota zip no cliente), rescan (14.02), UI (14.03), rollback (14.04), streaming acima do cap.

### Acceptance criteria

- [x] Install de zip válido retorna `201` com `{ name, version, url, kind, visibility }` e o worker responde na rota **sem restart**.
- [x] Key sem `workers:install` recebe `403`; sem key, `401`.
- [x] Zip com entry `../fora` ou path absoluto é rejeitado com `400` tipado e nada é escrito no root.
- [x] Zip sem entrypoint/manifest válido é rejeitado com `400` e diretório temporário é limpo.
- [x] Instalar `name@version` já indexado retorna `409 COLLISION`; nova versão do mesmo nome instala e coexiste.
- [x] Body acima do cap responde `413` (comportamento existente preservado).

### Dependencies

- Epic 08.02 (admin API + permissões), Epic 10.02 (padrão reconcile/validação operacional)

## Test-first plan

- **Behavior:** E2E pela pipeline HTTP real (`build_pipeline`), zip construído em memória no teste; primeiro teste é o caminho feliz install→fetch.
- **Level:** `crates/edger-orchestrator/tests/deploy_install.rs` + workspace gate.
- **Avoid:** testar helpers de zip isolados sem passar pela rota; mocks do índice.

## Tasks

### Fase 1 — Extração segura + validação
- [x] `deploy.rs`: extrair zip para tempdir sob o root com defesa zip-slip (entry canônica `starts_with` extração).
- [x] Validar worker extraído com `load_worker_manifest`; erros tipados (`DEPLOY_INVALID_PACKAGE`, `DEPLOY_PATH_DENIED`).
- [x] Teste unitário-integração dos negativos (zip-slip, sem entrypoint).

### Fase 2 — Rota + indexação
- [x] Roots de install disponíveis em runtime. (implementado como `ManifestIndex::set_roots/roots` gravados por `load_manifests_from_dirs` — sem quebrar construtores do `OrchestratorState`)
- [x] Rota `POST /api/admin/workers/install` com `workers:install`; colisão `409`.
- [x] Rename atômico + `ManifestIndex::insert` + resposta com URL do worker.

### Fase 3 — Prova E2E e docs
- [x] E2E: install zip → `GET /<worker>` responde 200 no mesmo processo.
- [x] E2E: 401/403/400/409/413.
- [x] Documentar fluxo em `docs/developers/06-operacao-e-testes.adoc`.

## Verification

```bash
cargo test -p edger-orchestrator --test deploy_install
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed** (2026-07-02) — `POST /api/admin/workers/install` entregue em
`crates/edger-orchestrator/src/deploy.rs`: extração zip com defesa zip-slip
(`enclosed_name`), unwrap de pasta top-level, validação canônica via
`load_worker_manifest`, check de namespace do principal, escrita atômica
(staging + rename) com rollback em colisão, indexação em runtime e resposta
`{name, version, url, kind, visibility, source}`. Permissão `workers:install`
+ CSRF guard. Evidência live:
`status/evidence/deploy-vertical-slice-2026-07-02.txt`.
