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
| `crates/edger-orchestrator/src/manifest_loader.rs` | create | `load_manifests_from_dirs`, index por nome/namespace/semver |
| `crates/edger-orchestrator/src/resolver.rs` | create/edit | Resolução de `WorkerRef` + `ExecutionKind` hint para pool |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Passar `kind_hint` e `WorkerConfig` normalizado ao `WorkerPool::fetch` |
| `crates/edger-orchestrator/src/router.rs` | edit | Integrar lookup de manifest após path resolution |
| `crates/edger-core/src/manifest.rs` | edit | Garantir campos `kind`, `cron`, `inject_base`, `public_routes` completos |
| `crates/edger-core/src/config.rs` | edit | `parse_worker_config` + normalização para `ExecutionKind` |
| `crates/edger-core/src/execution.rs` ou `lib.rs` | edit | Enum `ExecutionKind` completo (`FetchHandler`, `RoutesTable`, `StaticSpa`, `WasmModule`, `Fullstack`) |
| `crates/edger-worker/src/pool.rs` | edit | Aceitar e repassar `ExecutionKind` ao isolate |
| `crates/edger-isolation/src/kinds.rs` | edit | Match exhaustivo em todos os kinds (delegar para backends) |
| `crates/edger-orchestrator/tests/manifest_loader_test.rs` | create | Testes de multi-dir, colisão, inferência |
| `crates/edger-orchestrator/tests/kind_dispatch_integration.rs` | create | E2E por kind com fixtures em `workers/` |
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
- [x] Inferência cobre todos os 5 passos do design; `kind` explícito sobrescreve inferência. Passos 1/2/4/5 no loader (`infer_execution_kind`); passo 3 (exports `fetch`/`routes`) é resolvido em execução pela bridge Deno, que prioriza `routes` export sobre `fetch` (semântica Bun.serve).
- [x] Colisão de nome+namespace+versão retorna erro tipado (não sobrescreve silenciosamente). (`ManifestIndex::insert` → `COLLISION`; teste `insert_detects_collision`)
- [x] Cada `ExecutionKind` tem teste de integração que retorna resposta HTTP válida: FetchHandler (`js_worker_dispatches_through_deno_backend`), RoutesTable (`routes_table_worker_dispatches_by_path_method_and_params`), StaticSpa (`shell_routing_test.rs` + `manifest_loader.rs`), WasmModule (`same_process_serves_deno_and_wasm_workers_from_one_pool`), Fullstack 501 (`fullstack_worker_returns_501_adapter_required`).
- [x] `RUNTIME_WORKER_DIRS` com múltiplos paths (`dir1:dir2`) merge correto com precedência documentada.
- [x] Worker com `enabled: false` não é despachado.

### Dependencies
- Story 07.04 (JS backend para Fetch/Routes/SPA)
- Story 07.05 (Wasm backend)
- Epic 05 (pipeline HTTP, router, pool wiring)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`crates/edger-orchestrator/tests/`, `crates/edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Core + loader (vertical)
- [x] Completar/normalizar `ExecutionKind` e `parse_worker_config` em `edger-core` com testes de mapping Buntime.
- [x] Implementar `manifest_loader.rs`: scan dir, parse yaml/package/index, build `WorkerRef`, collision check.
- [x] Testes unitários: inferência por entrypoint, semver default `latest`, namespace parse.

### Fase 2 — Resolver + pipeline
- [x] Lookup por path (`/name`, `/@scope/name@ver`) usando registry. (entregue em `router.rs`; módulo `resolver.rs` separado desnecessário — testes em `routing_resolution.rs`)
- [x] Integrar loader no `main`/composition sketch do orchestrator (env `RUNTIME_WORKER_DIRS`, default `workers`).
- [x] Pipeline: após auth/hooks, resolver worker → `pool.fetch_worker(..., kind_hint)`. (`pipeline.rs::dispatch_worker`)

### Fase 3 — Dispatch exhaustivo
- [x] `crates/edger-isolation/kinds.rs`: branch por `ExecutionKind` chamando trait methods corretas.
- [x] Fixtures por kind. (repo: `workers/examples/hello-world` fetch, `workers/examples/routes-demo` routes, `workers/examples/wasm-hello` wasm, `workers/core/cpanel` SPA; fullstack coberto por fixture temp-dir no teste 501)
- [x] Integration tests E2E via tower/hyper test client. (`kind_dispatch_integration.rs`, `shell_routing_test.rs`)

### Fase 4 — Documentação e gate
- [x] Compat matrix atualizada (`routes` export → tested); design mapping sem gaps novos.
- [x] `cargo test --workspace` + clippy verde. (ver evidência do gate na validação final)

## Verification
```bash
cargo test -p edger-core -- manifest
cargo test -p edger-orchestrator -- manifest_loader
cargo test -p edger-orchestrator -- kind_dispatch
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed** (2026-07-02) — loader multi-dir, inferência com `kind` explícito
prioritário (incluindo respeito a `injectBase` no `kind: spa`), colisão tipada
e dispatch E2E de todos os `ExecutionKind` com backends reais: JS/TS via Deno
CLI bridge (fetch + routes), Wasm via wasmtime, Static SPA via
`serve_static_spa` e Fullstack documentado como 501 adapter-required.
