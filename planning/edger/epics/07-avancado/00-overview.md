# Epic 07: Features Avançadas, Observabilidade e Preparação (Fase 7)

**Origin:** `planning/edger/roadmap.md` (Fase 7), `planning/edger/design.md` (PR 10–12)

## Traceability
- **Source docs:** `planning/edger/design.md` (PR Plan 10–12, Observability, Rollout, Buntime mapping), `planning/edger/roadmap.md` (Fase 7), `planning/edger/analysis-synthesis.md` (disciplina, testes de integração)
- **Roadmap phase:** Fase 7 — Features Avançadas, Observabilidade e Preparação
- **Depends on epics:**
  - `planning/edger/epics/05-orquestrador/00-overview.md` (Fase 5: pipeline HTTP, routing, auth, hooks, servidor básico)
  - `planning/edger/epics/06-extensibilidade/00-overview.md` (Fase 6: registry estático, primeiras `edger-ext-*`)
- **Design PRs covered:** PR 10 (execução real JS/Wasm), PR 11 (manifests completos, shell, cron nativo), PR 12 (observabilidade, hardening, medição)

## Context

### Macro problem
Após Fases 5–6, o edger tem orquestrador funcional com mocks ou execução mínima, mas ainda não entrega o contrato Buntime completo: todos os `ExecutionKind`, manifests descobertos em diretórios, shell/micro-frontends, cron nativo, backends reais de isolamento (deno_core + wasmtime), observabilidade de produção e matriz de compatibilidade verificável.

### Initiative objective
Consolidar o runtime para uso real e migração Buntime: execução production-path, dispatch completo por kind, shell evoluído, scheduler Rust, OTEL/métricas, limites de segurança e harness de performance com baselines documentadas.

### Expected business/technical outcome
- Servidor edger atende workers JS/TS (fetch, routes, SPA), Wasm e kinds inferidos/explícitos a partir de `manifest.yaml` em `RUNTIME_WORKER_DIRS`.
- Cron dispara requisições internas via scheduler nativo (tokio-cron), não apenas stub.
- Shell serve micro-frontends com injeção de `<base href>` e notas de protocolo evoluído.
- `/metrics`, tracing estruturado e correlação `request_id` em todo o pipeline.
- Matriz de compatibilidade Buntime com testes automatizados + harness de perf (PR 12).

### Constraints, assumptions, and references
- JS/TS: Deno CLI bridge funcional em v1; `deno_core` + facade permanece o alvo embutido de produção.
- Wasm: `wasmtime` + WASI standalone (não co-localizado no isolate JS).
- Extensões: registro estático (inventory/linkme); sem dlopen em v1.
- Auth Turso/SQLite já wired nos épicos 05–06; verificar persistência aqui se gaps.
- Multi-process iniciado cedo (Fases 4–5); wire formats `Serialized*` já definidos em `edger-core`.
- Disciplina: `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check` antes de qualquer PR deste épico.
- O adapter Bun foi removido; não usar Bun como fallback de runtime. `bun test` só se aplica se uma suíte JS raiz for reintroduzida.

### AS-IS
- Orquestrador básico (épico 05) resolve paths e despacha para pool com mock ou execução parcial.
- Isolamento (Fase 3) tem spike e trait `Isolate` com mock cobrindo `ExecutionKind`.
- Manifest loader multi-dir e inferência de kind estão em progresso; Wasm já executa via pipeline Rust para `workers/examples/wasm-hello`.
- Cron nativo e `/metrics` existem em v1; observabilidade ainda precisava
  fechar correlação gerada, contadores HTTP e startup `OTEL_*` não fatal.
- PR 10–12 do design ainda não implementados.

### TO-BE
- `load_manifests_from_dirs` indexa workers por nome/namespace/semver com detecção de colisão.
- Pipeline despacha `FetchHandler` JS/TS e `WasmModule` para backends reais; variants restantes ainda dependem das stories específicas.
- `crates/edger-isolation/src/deno.rs` (facade) cobre fetch/routes/SPA; `wasmtime` cobre `WasmModule`.
- Shell routing com `inject_base` e documentação de evolução de protocolo (WebTransport etc.).
- `CronScheduler` em orchestrator com tokio-cron disparando HTTP interno autenticado.
- Tracing estruturado, env OTEL não fatal e endpoint Prometheus; limites
  body/header; testes de compat Buntime; harness de perf.

### Out of scope
- Deploy K8s/Helm, cpanel, marketplace de extensões.
- 100% compat Node / Next.js nativo sem adapters.
- Dynamic loading de crates Rust em runtime.
- Implementação completa de todos os plugins Buntime atuais (ficam como `edger-ext-*` futuros).
- Clustering multi-proc completo (apenas validação/notas; full rollout pós-fundação).

## Story backlog

| Story | Arquivo | Tamanho | Status | Depende de |
|---|---|---|---|---|
| 07.01 Manifests + kinds completos | `01-full-manifests-kinds.md` | large | **completed** (dispatch E2E por todos os kinds) | 07.04, 07.05, Epic 05 |
| 07.02 Shell routing | `02-shell-routing.md` | medium | **completed** (SPA namespaced + injectBase false + doc protocolo; decisão shell entregue na 08.05) | 07.01 |
| 07.03 Cron nativo | `03-native-cron.md` | medium | **completed** (tokio scheduler v1) | 07.01, Epic 05 |
| 07.04 Execução JS real | `04-real-js-execution.md` | large | **completed** (entregue pelo Epic 15: processo Deno persistente por UDS é o runtime JS durável de produção — ~25x vs bridge v1; a bridge v1 fica como fallback legado `EDGER_JS_RUNTIME=bridge`; streaming passthrough pelo Epic 16.D) | Epic 03 (spike), Epic 04, Epic 05 |
| 07.05 Execução Wasm | `05-wasm-execution.md` | large | **completed** (standalone wasmtime v1; ABI/WASI host follow-ups) | Epic 03 (spike), Epic 04, Epic 05 |
| 07.06 Observabilidade OTEL | `06-observability-otel.md` | medium | **completed** (observability v1; OTLP exporter pending) | 07.01, 07.04, 07.05 |
| 07.07 Hardening + matriz compat | `07-hardening-compat-matrix.md` | large | **completed** (limits, compat smoke, perf harness, CI v1) | 07.02, 07.03, 07.06 |

**Nota de sequência (caminho crítico):** PR 10 (stories 07.04 + 07.05 em paralelo) desbloqueia PR 11 (07.01 → 07.02/07.03). PR 12 fecha com 07.06 → 07.07.

## Epic roadmap

```mermaid
flowchart LR
    E05[Epic 05 Orquestrador] --> S04[07.04 JS real]
    E06[Epic 06 Extensões] --> S04
    E05 --> S05[07.05 Wasm]
    S04 --> S01[07.01 Manifests + kinds]
    S05 --> S01
    S01 --> S02[07.02 Shell]
    S01 --> S03[07.03 Cron]
    S02 --> S06[07.06 OTEL]
    S03 --> S06
    S04 --> S06
    S05 --> S06
    S06 --> S07[07.07 Hardening + compat]
```

### Fases sugeridas

| Fase | Stories | Validação intermediária |
|---|---|---|
| A — Execução real (PR 10) | 07.04, 07.05 (paralelo) | Integration test: fetch JS + módulo Wasm respondem via pool |
| B — Contratos completos (PR 11) | 07.01 → 07.02 + 07.03 | E2E: manifest multi-dir, SPA base injection, cron tick |
| C — Produção foundation (PR 12) | 07.06 → 07.07 | `/metrics` scrape, matriz compat verde, baselines no harness |

## Epic acceptance criteria
- [x] `load_manifests_from_dirs` carrega workers de `RUNTIME_WORKER_DIRS` (`:`) com inferência de `ExecutionKind` e colisão detectada.
- [x] Todos os variants de `ExecutionKind` despacham para backend correto (JS via Deno CLI bridge/deno_core target; Wasm via wasmtime; StaticSpa com `inject_base`; Fullstack 501 adapter-required).
- [x] Shell routing serve HTML com `<base href>` quando `inject_base: true`; notas de protocolo evoluído documentadas.
- [x] Cron nativo dispara requisições internas conforme `manifest.cron[]`; testes cobrem schedule v1 + auth interna.
- [x] Tracing estruturado com `request_id` em orchestrator -> pool -> isolate;
  env OTEL configurável sem falhar startup; `/metrics` Prometheus.
- [x] Limites body/header (port Buntime HeaderLimits) enforced no pipeline.
- [x] Matriz de compatibilidade Buntime com testes automatizados passando (`tests/compat/` ou equivalente).
- [x] Harness de performance define baselines documentadas em PR 12.
- [x] `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check` verde. (`status/evidence/`)
- [x] Gate de planejamento verde; JS root test gate registrado como skipped ou passing conforme existir suíte JS raiz.

## Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Spike deno_core revela custo de manutenção alto | High | Time-box; feature flags; facade module isolado; pin de versões |
| Wasm WASI capabilities mal configuradas | Medium | Testes com fixtures mínimos; deny-by-default; validação de módulo |
| Drift de contratos Buntime durante dispatch multi-kind | Medium | Matriz explícita na story 07.07; testes por campo do mapping table |
| Cron + auth interna expõe superfície de ataque | Medium | Credencial interna `X-Buntime-Internal` equivalente; rotas cron não públicas |
| OTEL overhead em hot path | Low | Sampling configurável; spans leves no pool hit |
| Story 07.01 antes de backends reais gera falsa sensação de done | Medium | Gate: 07.01 só “done” com 07.04+07.05 verdes em integração |

## Recommended next step
- Após conclusão dos épicos 05 e 06: `/agile-story` em `04-real-js-execution.md` (desbloqueia caminho crítico PR 10).
- Paralelamente possível: `/agile-story` em `05-wasm-execution.md`.
- Ao fechar o épico: `/agile-refinement` + atualizar `planning/edger/roadmap.md` (Fase 7 → done).

## Status
**functional-complete** (2026-07-02) — 07.01, 07.02, 07.03, 07.05, 07.06 e 07.07 completed; 07.04 in progress como Deno CLI bridge v1 **sandboxed** (`deno run` com `--allow-read=<worker_dir>`, write/run/ffi/sys negados, net configurável via `EDGER_DENO_ALLOW_NET`) com dispatch real de `routes` export e recuperação de pool após erro de isolate. Pendências que seguem em aberto (todas fora do caminho funcional): `deno_core` embedded boot (aguarda aprovação explícita), full Wasm request-memory ABI + host WASI, OTLP exporter real, perf scenarios expandidos e Turso auth/argon2 (carry 06.02). Pendências: `docs/pendencies-epic-07.md`

Plano funcional ativo: `planning/edger/runtime-functional-plan.md` (MVP funcional validado ao vivo em 2026-07-02; evidência em `status/evidence/`).
