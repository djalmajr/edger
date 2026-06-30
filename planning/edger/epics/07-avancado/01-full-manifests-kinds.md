# Story 07.01: Manifests completos e dispatch de todos os ExecutionKind

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`

## Context
- **Problema:** O orquestrador resolve apenas subset de workers; carregamento multi-dir e inferência completa de `ExecutionKind` não cobrem o contrato Buntime (serverless, SPA, routes, Wasm, fullstack adapter).
- **Objetivo:** Implementar descoberta de manifests em diretórios e dispatch unificado de todos os `ExecutionKind` do `edger-core` até o pool/isolamento.
- **Valor:** Workers deployáveis como no Buntime — um manifest define comportamento; o runtime infere ou honra `kind` explicitamente.
- **Restrições:** Depende de backends reais (stories 07.04/07.05) para validação E2E; parsing permanece em `edger-core` (sem I/O).

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 11, Data Model, App/Worker Types, inference rules)
- **Design PR:** PR 11 (parcial — manifests + kinds; shell e cron em stories separadas)
- **Buntime refs:** `planning/edger/design.md (contratos runtime; ai-memory zommehq/buntime)` (manifests, inference), `planning/edger/design.md (mapping table)`
- **Depende de:** `04-real-js-execution.md`, `05-wasm-execution.md`, Epic 05 (pipeline + router)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/manifest_loader.rs` | create | `load_manifests_from_dirs`, index por nome/namespace/semver |
| `edger-orchestrator/src/resolver.rs` | create/edit | Resolução de `WorkerRef` + `ExecutionKind` hint para pool |
| `edger-orchestrator/src/pipeline.rs` | edit | Passar `kind_hint` e `WorkerConfig` normalizado ao `WorkerPool::fetch` |
| `edger-orchestrator/src/router.rs` | edit | Integrar lookup de manifest após path resolution |
| `edger-core/src/manifest.rs` | edit | Garantir campos `kind`, `cron`, `inject_base`, `public_routes` completos |
| `edger-core/src/config.rs` | edit | `parse_worker_config` + normalização para `ExecutionKind` |
| `edger-core/src/execution.rs` ou `lib.rs` | edit | Enum `ExecutionKind` completo (`FetchHandler`, `RoutesTable`, `StaticSpa`, `WasmModule`, `Fullstack`) |
| `edger-worker/src/pool.rs` | edit | Aceitar e repassar `ExecutionKind` ao isolate |
| `edger-isolation/src/kinds.rs` | edit | Match exhaustivo em todos os kinds (delegar para backends) |
| `edger-orchestrator/tests/manifest_loader_test.rs` | create | Testes de multi-dir, colisão, inferência |
| `edger-orchestrator/tests/kind_dispatch_integration.rs` | create | E2E por kind com fixtures em `workers/` |
| `workers/manifest-kinds/` (fixtures) | create | Exemplos por kind para testes |

## Detail

### AS-IS
- `WorkerManifest` parcial em `edger-core`; router resolve path mas kind pode ser fixo ou mock.
- Sem `load_manifests_from_dirs`; env `RUNTIME_WORKER_DIRS` não indexado em startup.
- Regras de inferência do design (`.html` → SPA, `fetch` → FetchHandler, etc.) não totalmente implementadas.

### TO-BE
- Startup (ou reload) varre dirs separados por `:`; parse `manifest.yaml` / `package.json` fallback.
- Mapa `HashMap<WorkerKey, WorkerRef>` com namespace, semver (`latest`), colisão detectada com erro claro.
- Inferência: (1) `kind` explícito no manifest; (2) sufixo `.html`; (3) exports `fetch`/`routes`; (4) `.wasm`; (5) default `FetchHandler`.
- Pipeline passa `ExecutionKind` ao pool; isolate executa método correto (`execute_fetch`, `execute_routes`, `serve_static_spa`, `execute_wasm`, adapter fullstack).
- `enabled: false` no manifest exclui worker do registry sem restart (toggle Buntime).

### Scope
- **In:** Loader multi-dir, inferência, dispatch exhaustivo, testes integração por kind, colisão/semver.
- **Out:** Shell UI routing (07.02), cron firing (07.03), OTEL (07.06), limites body (07.07).

### Acceptance criteria
- [x] `load_manifests_from_dirs(&[PathBuf])` retorna registry com workers namespaced (`@scope/name@ver`).
- [ ] Inferência cobre todos os 5 passos do design; `kind` explícito sobrescreve inferência. Parcial: manifest/package/index + `.html`/`.wasm`/`.wat`.
- [ ] Colisão de nome+namespace+versão retorna erro tipado (não sobrescreve silenciosamente).
- [ ] Cada `ExecutionKind` tem teste de integração que retorna resposta HTTP válida (parcial: Wasm real coberto por `kind_dispatch_integration.rs`; JS aguarda 07.04).
- [x] `RUNTIME_WORKER_DIRS` com múltiplos paths (`dir1:dir2`) merge correto com precedência documentada.
- [x] Worker com `enabled: false` não é despachado.

### Dependencies
- Story 07.04 (JS backend para Fetch/Routes/SPA)
- Story 07.05 (Wasm backend)
- Epic 05 (pipeline HTTP, router, pool wiring)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`edger-orchestrator/tests/`, `edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Core + loader (vertical)
- [x] Completar/normalizar `ExecutionKind` e `parse_worker_config` em `edger-core` com testes de mapping Buntime.
- [x] Implementar `manifest_loader.rs`: scan dir, parse yaml/package/index, build `WorkerRef`, collision check.
- [x] Testes unitários: inferência por entrypoint, semver default `latest`, namespace parse.

### Fase 2 — Resolver + pipeline
- [ ] `resolver.rs`: lookup por path (`/name`, `/@scope/name@ver`) usando registry.
- [x] Integrar loader no `main`/composition sketch do orchestrator (env `RUNTIME_WORKER_DIRS`, default `workers`).
- [ ] Pipeline: após auth/hooks, resolver worker → `pool.fetch(..., kind_hint)`.

### Fase 3 — Dispatch exhaustivo
- [ ] `edger-isolation/kinds.rs`: branch por `ExecutionKind` chamando trait methods corretas.
- [ ] Fixtures em `workers/manifest-kinds/` (fetch, routes, spa, wasm, fullstack stub).
- [ ] Integration tests E2E via tower/hyper test client (parcial: Wasm real verde).

### Fase 4 — Documentação e gate
- [ ] Atualizar comentários em `design.md` mapping table se gaps encontrados.
- [ ] `cargo test --workspace` + clippy verde.

## Verification
```bash
cargo test -p edger-core -- manifest
cargo test -p edger-orchestrator -- manifest_loader
cargo test -p edger-orchestrator -- kind_dispatch
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
