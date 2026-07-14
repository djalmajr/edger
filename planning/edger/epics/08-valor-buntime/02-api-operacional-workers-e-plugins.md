# Story 08.02: API operacional para workers, extensões e chaves

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Status
completed (2026-06-29) — API operacional v1 entregue com inventário root-only de workers, extensões e keys, endpoint de sessão para key válida e mutações protegidas. As mutações nasceram como `501` tipado nesta story; 08.11 substituiu worker enable/disable por overlay runtime real e 08.13 fez o mesmo para extensões. Persistência segura ainda é futura.

## Context
- **Problema:** Buntime entrega valor operacional por APIs de workers, plugins, arquivos e chaves. edger ainda depende de configuração local e execução manual para provar runtime.
- **Objetivo:** Expor uma API Rust-native para inspeção e alteração controlada de workers, extensões e chaves, sem assumir uma UI final.
- **Valor:** Operadores conseguem administrar o runtime, automatizar deploy local e validar estado sem mexer diretamente nos diretórios.
- **Restrições:** Mutação só com auth forte; namespace e escopo devem estar prontos para a story de segurança; não clonar rotas internas Buntime quando um contrato edger mais claro bastar.

## Traceability
- **Source docs:** `planning/edger/epics/07-avancado/01-full-manifests-kinds.md`, `planning/edger/docs/value-parity-matrix.md`
- **Buntime refs:** runtime API, worker routes, plugin routes e key routes em `<buntime-repo>/apps/runtime/src/`
- **Prototype refs:** none.
- **Business rules:** API primeiro; UI administrativa é entrega posterior.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/admin.rs` | create | Tipos puros para requests/responses administrativos |
| `crates/edger-core/src/lib.rs` | edit | Exportar vocabulário admin sem I/O |
| `crates/edger-core/src/api_key_store.rs` | edit | Adicionar inventário seguro de metadata de keys |
| `crates/edger-orchestrator/src/admin_api.rs` | create | Rotas HTTP administrativas |
| `crates/edger-orchestrator/src/lib.rs` | edit | Exportar módulo admin API |
| `crates/edger-orchestrator/src/bin/edger.rs` | no change | O binário já usa `build_pipeline`; a montagem ficou no pipeline |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Montar rotas `/api/admin/*` antes do fallback de workers |
| `crates/edger-orchestrator/src/manifest_loader.rs` | no change | O índice atual já carrega a fonte usada pela API operacional |
| `crates/edger-orchestrator/src/manifest_index_stub.rs` | edit | Expor listagem ordenada de workers/plugins sem vazar storage interno |
| `crates/edger-orchestrator/src/registry.rs` | edit | Expor inventário estável de extensões registradas |
| `edger-ext-auth/src/lib.rs` | edit | Expor inventário seguro de keys via contrato administrativo |
| `edger-ext-auth/src/store.rs` | edit | Implementar listagem de metadata sem hash/key raw |
| `crates/edger-orchestrator/tests/admin_workers_plugins.rs` | create | Testes de listagem, inspeção e mutação protegida |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar comandos e contratos administrativos |
| `planning/edger/docs/value-parity-matrix.md` | edit | Marcar linhas de gestão operacional com evidência |

## Detail

### AS-IS
- O binário carrega worker roots e serve pipeline, mas não expõe inventário operacional completo.
- Chaves e extensões existem como conceitos em crates, sem API administrativa consolidada.
- A prova atual depende de curl e inspeção manual.

### TO-BE
- Admin API lista workers descobertos, manifests, status de runtime, extensões registradas e chaves configuradas.
- V1 é root-only para endpoints administrativos; namespace-scoped admin fica na Story 08.03 para não misturar desenho de API com política de autorização incompleta.
- Mutações mínimas entram como rotas protegidas que não alcançam worker dispatch. Quando persistência segura de manifest ainda não existir, retornam erro tipado em vez de editar arquivo de forma frágil.
- Respostas usam tipos de `edger-core`, mantendo core sem I/O.
- Erros são tipados e não vazam detalhes internos.

### Approach

| Decisão story-time | Escolha | Motivo |
|---|---|---|
| Prefixo | `/api/admin/*` | Evita colisão com worker paths e preserva `/api` como superfície runtime reservada |
| Auth v1 | Root-only | Mais seguro até 08.03 fechar namespace/admin permissions |
| Inventário de workers | Métodos de listagem em `ManifestIndex` | Não expõe `HashMap` interno nem duplica parsing |
| Inventário de extensões | `ExtensionRegistry` retorna metadata ordenada | Reusa registry estático atual e evita plugin loader novo |
| Inventário de keys | Metadata sem segredo/hash | Cumpre valor operacional sem vazar raw key |
| Mutação v1 | Protegida e tipada, podendo retornar `501` quando persistência não está pronta | Prova auth/roteamento sem fingir enable/disable seguro |

### Risks
- Adicionar `list_keys` diretamente ao `ApiKeyStore` força atualização de todos os implementadores; hoje só existe `SqliteApiKeyStore`, então o impacto é aceitável.
- Se o admin router for montado depois do fallback, `/api/admin/*` volta a cair em `API_STUB`; teste E2E deve matar essa regressão.
- Root-only v1 é deliberadamente conservador; a Story 08.03 deve expandir para namespace/permission-aware.

### Scope
- **In:** listagem, inspeção, health operacional, mutação mínima protegida, testes de auth.
- **Out:** upload de pacotes, marketplace, UI CPanel, reload distribuído multi-pod.

### Acceptance criteria
- [x] `GET /api/admin/workers` retorna workers, namespace, versão, kind, source e status.
- [x] `GET /api/admin/extensions` retorna extensões registradas e providers declarados.
- [x] `GET /api/admin/keys` não vaza segredo bruto e exige autorização root.
- [x] Mutação administrativa exige auth e retorna 401/403 corretos sem dispatch para worker.
- [x] Teste de integração cobre sucesso e negação.
- [x] Matriz de valor aponta a evidência da API operacional.

### Dependencies
- Story 08.01 para matriz e prioridade.
- Epic 07.01 para manifest loader/index estável.

## Test-first plan
- **Behavior:** `/api/admin/*` deve ser servido pelo admin router, exigir root auth e nunca cair no worker dispatch ou `API_STUB`.
- **First failing test:** `GET /api/admin/workers` sem auth retorna 401; com root retorna JSON com worker `name`, `namespace`, `version`, `kind`, `source`, `status`.
- **Preferred level:** integração em `crates/edger-orchestrator/tests/admin_workers_plugins.rs` usando `build_pipeline` e estado real com `ManifestIndex`, `ExtensionRegistry` e `SqliteApiKeyStore`.
- **Mutation captured:** mover admin routes para depois do fallback, remover root check ou retornar raw key deve quebrar testes.
- **Avoid:** testes que apenas confirmam structs serializam; a prova precisa ser resposta HTTP observável e ausência de segredo.

## Tasks
- [x] Fase 1 — Contratos puros.
  - Done when: `crates/edger-core/src/admin.rs` definir `AdminWorkerInfo`, `AdminExtensionInfo`, `AdminApiKeyInfo`, envelope de erro/health e reexport em `lib.rs`.
- [x] Fase 2 — Inventário de fontes atuais.
  - Done when: `ManifestIndex` listar workers/plugins de forma ordenada; `ExtensionRegistry` listar extensões; `SqliteApiKeyStore` listar key metadata sem key/hash.
- [x] Fase 3 — Admin router root-only.
  - Done when: `crates/edger-orchestrator/src/admin_api.rs` servir `/api/admin/workers`, `/api/admin/extensions`, `/api/admin/keys`, `/api/admin/session` e mutações protegidas mínimas.
- [x] Fase 4 — Integração no pipeline.
  - Done when: `build_pipeline` montar admin routes antes do fallback e `/api/admin/*` não retornar `API_STUB`.
- [x] Fase 5 — Testes E2E de API operacional.
  - Done when: `admin_workers_plugins.rs` cobrir sucesso root, 401 sem key, 403 non-root para keys, inventário de workers/extensions e negação de mutação sem dispatch.
- [x] Fase 6 — Documentação e matriz.
  - Done when: docs operacionais incluírem exemplos curl e `value-parity-matrix.md` marcar API operacional como `partial` ou `tested` conforme evidência.

## Verification
```bash
cargo test -p edger-orchestrator --test admin_workers_plugins
rg -n "api/admin|Admin API|API operacional" docs/developers planning/edger/docs/value-parity-matrix.md
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
