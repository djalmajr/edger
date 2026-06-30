# Story 08.13: Enable/disable runtime de extensoes

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz ainda marcava `APIs de plugins/extensoes` como `partial` porque o inventario existia, mas enable/disable real de extensoes ainda nao afetava hooks ou providers em runtime.
- **Objetivo:** Permitir que o operador desabilite e reabilite uma extensao registrada sem restart, com inventario atualizado e efeito real no pipeline.
- **Valor:** Captura o aprendizado do Buntime de controle operacional de plugins sem copiar o loader dinamico: edger usa registro estatico de crates, mas entrega um overlay runtime seguro para tirar capacidades de trafego.
- **Restricoes:** Sem edicao de manifest em disco, upload, rescan, hot reload persistente, UI ou marketplace nesta fatia.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/extensions.md`, `planning/edger/epics/08-valor-buntime/02-api-operacional-workers-e-plugins.md`
- **Buntime refs:** `/Users/djalmajr/Developer/djalmajr/buntime/apps/site/src/content/docs/concepts/plugin-system.mdx`, especialmente enable/disable runtime e a separacao entre reload persistente e superfice dinamica por request.
- **Prototype refs:** none; this is an operator API workflow.
- **Business rules:** extensoes continuam registradas explicitamente; toggles em memoria nao devem fingir persistencia nem clonar o modelo Bun.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/admin.rs` | edit | Adicionar `status` ao inventario puro de extensoes |
| `edger-orchestrator/src/registry.rs` | edit | Adicionar overlay compartilhado de status e providers/hooks ativos |
| `edger-orchestrator/src/hooks.rs` | edit | Executar somente middlewares ativos |
| `edger-orchestrator/src/admin_api.rs` | edit | Expor `POST /api/admin/extensions/{name}/enable|disable` |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Cobrir API protegida e inventario atualizado |
| `edger-orchestrator/tests/registry_hooks.rs` | edit | Provar que middleware desabilitado deixa de interceptar sem rebuild |
| `edger-orchestrator/tests/registry_providers.rs` | edit | Provar que provider desabilitada sai do lookup de bindings |
| `planning/edger/docs/value-parity-matrix.md` | edit | Atualizar evidencia e lacuna restante de reload/persistencia |
| `planning/edger/docs/compat-matrix.md` | edit | Registrar compatibilidade tecnica de extension runtime toggle |
| `planning/edger/docs/extensions.md` | edit | Documentar controle runtime e limites |
| `planning/edger/status/evidence/story-08-13-runtime.txt` | create | Capturar comandos e resultados |

## Detail

### AS-IS
- `ExtensionRegistry` guardava middlewares e providers em estruturas imutaveis por `Arc`.
- `GET /api/admin/extensions` listava extensoes registradas, mas sem status operacional.
- Hooks e bindings sempre consideravam toda extensao registrada.
- O Buntime tem `enable`/`disable` persistente por manifest + rescan; edger ainda nao tem loader dinamico equivalente.

### TO-BE
- O registry mantem todas as extensoes registradas, mas passa a ter um overlay `enabled`/`disabled` compartilhado entre clones.
- Inventario admin retorna `status` para cada extensao.
- `POST /api/admin/extensions/{name}/disable` muda o overlay em memoria, exige root e passa pela mesma protecao CSRF/internal-call das outras mutacoes.
- Middlewares desabilitados nao rodam `on_request`, `on_response`, `on_init`, `on_server_start` ou `on_shutdown`.
- Service providers desabilitados deixam de satisfazer `resolve_service_bindings`.

### Scope
- **In:** toggle em memoria, inventario com status, hooks ativos, provider lookup ativo, testes de API/registry.
- **Out:** persistir `enabled` no manifest, upload/rescan, rebuild topologico dinamico, reload de rotas nativas, marketplace, UI.

### Approach
- Adicionar `extension_status: Arc<RwLock<BTreeMap<String, bool>>>` no `ExtensionRegistry`.
- Preservar `middlewares()` como lista registrada e adicionar `active_middlewares()` para execucao.
- Fazer accessors de providers retornarem `None` quando a extensao estiver desabilitada, mantendo accessors internos para inventario.
- Reusar `AdminMutationResponse` para enable/disable de extensoes.

### Risks
- **Inventario cosmetico:** testes garantem que toggle muda hooks e provider lookup, nao apenas JSON.
- **Persistencia ambigua:** documentos deixam explicito que overlay e em memoria.
- **Auth lockout:** Admin API continua usando `AuthGate` ja montado; esta story nao troca o provedor de autenticacao operacional.

### Acceptance criteria
- [x] `GET /api/admin/extensions` retorna `status` por extensao.
- [x] `POST /api/admin/extensions/{name}/disable` e `/enable` exigem root e CSRF valido.
- [x] Desabilitar middleware remove seus hooks do pipeline ja construido.
- [x] Desabilitar service provider remove sua capability do lookup de bindings.
- [x] Matriz diferencia enable/disable runtime testado de reload/persistencia futura.

## Test-first plan
- First failing tests:
  - API: desabilitar `gateway` atualiza inventario para `disabled`, sem auth retorna 401 e origin hostil retorna `CSRF_DENIED`.
  - Hooks: middleware `teapot` short-circuita antes do toggle e deixa o pool responder depois do toggle, sem rebuild do router.
  - Providers: desabilitar `keyval` remove `provider:keyValue` e `provider:queue` do lookup.
- Preferred levels:
  - `edger-orchestrator/tests/admin_workers_plugins.rs`
  - `edger-orchestrator/tests/registry_hooks.rs`
  - `edger-orchestrator/tests/registry_providers.rs`
- Low-value tests avoided: serializacao isolada de structs ou teste que apenas chama setter sem observar pipeline.

## Tasks
- [x] Expandir contrato administrativo de extensoes.
  - Done when: `AdminExtensionInfo` inclui `status` e o inventario preserva extensoes registradas mesmo desabilitadas.
- [x] Implementar overlay runtime no registry.
  - Done when: hooks e providers consultam apenas capacidades ativas.
- [x] Expor endpoints de mutacao.
  - Done when: admin API tem `/api/admin/extensions/{name}/enable|disable` com root + CSRF/internal-call guard.
- [x] Adicionar testes focados.
  - Done when: API, hooks e providers provam efeito runtime.
- [x] Atualizar artefatos de planejamento.
  - Done when: matrizes, overview, README, docs, evidencia e closure mencionam 08.13.
- [x] Rodar gates.
  - Done when: testes focados, Rust gate e planning gate passam.

## Verification
```bash
cargo test -p edger-orchestrator --test registry_hooks
cargo test -p edger-orchestrator --test registry_providers
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
