# Story 07.04: Execução JS/TS real (deno_core + facade)

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`

## Context
- **Problema:** Após o spike (Fase 3), o isolate ainda usa mock; não há caminho production para `fetch`, `routes` e SPA estática via deno_core + facade como no Edge Runtime.
- **Objetivo:** Implementar backend real em `edger-isolation` usando deno_core + facade para `ExecutionKind` JS (FetchHandler, RoutesTable, StaticSpa), integrado ao WorkerPool com limites básicos e suporte eszip/precomp onde aplicável.
- **Valor:** Workers JS/TS Buntime-compat rodam no runtime Rust; desbloqueia PR 11 e testes E2E reais.
- **Restrições:** Bloqueado até spike go/no-go; feature flag `deno` no crate isolation; partial Node compat documentado; multi-process prep via `Serialized*` wire types.

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 10, Embedding Spike, Execution Isolation Layer, eszip/precomp)
- **Design PR:** PR 10 (parte JS/TS)
- **Edge Runtime refs:** `deno_facade`, `base_rt` patterns
- **Depende de:** Epic 03 (spike), Epic 04 (WorkerPool), Epic 05 (orchestrator dispatch)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/deno.rs` | create | Facade deno_core: boot isolate, ops, module load |
| `edger-isolation/src/deno/fetch.rs` | create | `execute_fetch` — export `fetch(req) -> Response` |
| `edger-isolation/src/deno/routes.rs` | create | `execute_routes` — tabela de rotas serializada |
| `edger-isolation/src/deno/static_spa.rs` | create | `serve_static_spa` — arquivos estáticos + base injection |
| `edger-isolation/src/bundle.rs` | create | Hooks eszip/precomp (carregar bundle ou arquivos) |
| `edger-isolation/src/limits.rs` | edit | Timeout, memory cap stubs → enforcement real |
| `edger-isolation/src/lib.rs` | edit | `DenoIsolate` impl `Isolate` trait; feature flag |
| `edger-isolation/Cargo.toml` | edit | deps `deno_core`, `deno_runtime` subset, optional features |
| `edger-worker/src/instance.rs` | edit | Spawn `DenoIsolate` no supervisor |
| `edger-worker/src/supervisor.rs` | edit | Resource limits na criação/destruição |
| `edger-isolation/tests/js_fetch_integration.rs` | create | Roundtrip request/response real |
| `edger-isolation/tests/js_routes_integration.rs` | create | Routes table handler |
| `edger-isolation/tests/js_spa_integration.rs` | create | HTML estático servido |
| `workers/js-fetch/`, `workers/js-routes/`, `workers/js-spa/` | create | Fixtures mínimas |
| `planning/edger/epics/03-isolacao-execucao/spike.md` | read | Go/no-go e sharp edges do spike |

## Detail

### AS-IS
- `Isolate` trait com `MockIsolate` cobrindo todos os kinds.
- Spike documentado mas sem código production em `src/deno.rs`.
- Pool chama mock; latência/spawn não medidos com V8 real.

### TO-BE
- `DenoIsolate` carrega entrypoint do worker dir (file ou eszip bundle).
- `execute_fetch`: serializa `SerializedRequest` → JS `fetch` handler → `SerializedResponse`.
- `execute_routes`: despacha por método/path na tabela exportada.
- `serve_static_spa`: lê filesystem do worker dir com path traversal prevention; injeta `<base href>` se configurado.
- Supervisor aplica timeout_ms do manifest; memory accounting básico (port patterns cpu_timer/mem_check).
- WorkerPool `fetch` usa backend real quando feature `deno` habilitada (default em bin release).
- Testes com fixtures em `workers/` espelhando contratos Buntime (`export default { fetch }`, Deno.serve style onde aplicável).

### Scope
- **In:** deno_core facade production path, fetch/routes/SPA, pool integration, limites básicos, testes integração.
- **Out:** Wasm (07.05), fullstack/SSR adapters (stub `Fullstack` retorna 501 com mensagem), 100% Node polyfills.

### Acceptance criteria
- [ ] Worker `workers/js-fetch/` responde GET com body esperado via `pool.fetch` + `DenoIsolate`.
- [ ] Worker `workers/js-routes/` roteia POST `/api/x` corretamente.
- [ ] SPA fixture retorna `text/html` com status 200; com `inject_base` injeta tag base.
- [ ] Timeout do manifest encerra execução longa com erro tipado `IsolationError::Timeout`.
- [ ] Path traversal em entrypoint/static rejeitado (`../` fora do worker dir).
- [ ] `cargo test -p edger-isolation` verde com feature `deno`; mock ainda disponível com `--no-default-features` para CI rápido se necessário.
- [ ] Spawn + exec baseline registrado em log (prep para harness 07.07).

### Dependencies
- Epic 03 — spike deno_core concluído com go
- Epic 04 — WorkerPool + Supervisor
- Epic 05 — orchestrator chama pool (pode usar mock até wiring)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`edger-orchestrator/tests/`, `edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Facade bootstrap
- [ ] Criar `deno.rs`: V8 platform init (singleton), op registration mínima, load module from path.
- [ ] Roundtrip hello-world fetch (string module) — teste isolado.
- [ ] Documentar sharp edges do spike em comentários/module docs.

### Fase 2 — ExecutionKind JS
- [ ] `execute_fetch`: bridge `SerializedRequest`/`Response` JS ↔ Rust.
- [ ] `execute_routes`: convenção de export `routes` (objeto ou array) alinhada Buntime.
- [ ] `serve_static_spa`: fs read + MIME + base injection.

### Fase 3 — Pool + limits
- [ ] `DenoIsolate` wired em `edger-worker/instance.rs`.
- [ ] Supervisor: timeout timer, terminate on critical error.
- [ ] `limits.rs`: port inicial de memory/CPU guard do spike.

### Fase 4 — Bundling + testes
- [ ] `bundle.rs`: suporte file-based v1; hook eszip se spike recomendou.
- [ ] Fixtures `workers/js-*` + integration tests.
- [ ] Gate workspace; atualizar README com feature flags.

## Verification
```bash
cargo test -p edger-isolation --features deno
cargo test -p edger-worker --features deno
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
bun test
```
