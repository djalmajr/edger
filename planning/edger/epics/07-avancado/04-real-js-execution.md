# Story 07.04: Execução JS/TS real (Deno bridge + deno_core facade)

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`

## Context
- **Problema:** Após o spike (Fase 3), o isolate ainda usava mock para JS/TS; o adapter Bun foi removido, então não havia mais caminho alternativo para executar exemplos JS/TS fora do runtime Rust. Falta o backend production embutido para `fetch`, `routes` e SPA estática via deno_core + facade como no Edge Runtime.
- **Objetivo:** Implementar backend real em `edger-isolation` para `ExecutionKind` JS. V1 usa Deno CLI bridge para tornar o runtime funcional; a versão final troca o bridge por deno_core + facade.
- **Valor:** Workers JS/TS Buntime-compat rodam no runtime Rust; desbloqueia PR 11 e testes E2E reais.
- **Restrições:** Bloqueado até spike go/no-go; feature flag `deno` no crate isolation; partial Node compat documentado; multi-process prep via `Serialized*` wire types.

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 10, Embedding Spike, Execution Isolation Layer, eszip/precomp)
- **Plano ativo:** `planning/edger/runtime-functional-plan.md`
- **Design PR:** PR 10 (parte JS/TS)
- **Edge Runtime refs:** `deno_facade`, `base_rt` patterns
- **Depende de:** Epic 03 (spike), Epic 04 (WorkerPool), Epic 05 (orchestrator dispatch)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/deno/cli.rs` | create | Bridge funcional via Deno CLI para Deno.serve/default fetch |
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
- `DenoIsolate` executa JS/TS real via Deno CLI bridge.
- Exemplos principais em `workers/` passam por `edger-orchestrator` + `WorkerPool`.
- Worker-local `deno.json`/`deno.jsonc` é carregado pela bridge; import maps locais cobertos por teste.
- `stream`/`sse` têm resposta bounded-first-chunk no bridge v1; passthrough streaming real segue pendente.
- `logger-stdout`, `serve` e `commonjs` respondem manualmente via bin Rust; `commonjs-hono` funciona quando chamado com a rota interna como subpath montado, que é a semântica Buntime de strip-prefix + `x-base`.
- Static SPA real (`entrypoint: index.html`) serve HTML/assets/fallback e injeta `<base href>`.
- `buntime/apps/todos` foi validado por HTTP sem `Authorization`; Chrome automation ficou bloqueada por indisponibilidade do backend Chrome.
- Spike `deno_core` segue documentado como caminho embutido pendente.

### TO-BE
- `DenoIsolate` carrega entrypoint do worker dir (file ou eszip bundle).
- `execute_fetch`: serializa `SerializedRequest` → JS `fetch` handler → `SerializedResponse`.
- `execute_routes`: despacha por método/path na tabela exportada.
- `serve_static_spa`: lê filesystem do worker dir com path traversal prevention; injeta `<base href>` se configurado.
- Supervisor aplica timeout_ms do manifest; memory accounting básico (port patterns cpu_timer/mem_check).
- WorkerPool `fetch` usa backend real quando feature `deno` habilitada (default em bin release).
- Testes com fixtures em `workers/` espelhando contratos Buntime (`export default { fetch }`, Deno.serve style onde aplicável).

### Scope
- **In:** Deno CLI bridge funcional, deno_core facade production path, fetch/routes/SPA, pool integration, limites básicos, testes integração.
- **Out:** Wasm (07.05), fullstack/SSR adapters (stub `Fullstack` retorna 501 com mensagem), 100% Node polyfills.

### Acceptance criteria
- [x] Worker fetch responde com body esperado via pipeline + `DenoIsolate`.
- [x] Worker com import remoto (`logger-stdout`) responde via validação manual.
- [x] Worker com `deno.json`/JSR (`serve`) responde via validação manual.
- [x] CommonJS `http.createServer(...).listen(...)` simples responde via adapter Node mínimo.
- [x] Worker montado recebe path relativo e header `x-base` compatíveis com Buntime.
- [x] Static SPA real serve index/assets/fallback com base injection.
- [x] `buntime/apps/todos` responde sem auth em `/todos` e assets.
- [ ] Worker `workers/js-routes/` roteia POST `/api/x` corretamente.
- [ ] SPA fixture retorna `text/html` com status 200; com `inject_base` injeta tag base.
- [x] Timeout do manifest encerra execução longa com erro tipado (`DENO_TIMEOUT`).
- [x] `stream`/`sse` retornam primeiro chunk/evento sem travar o request no bridge v1.
- [ ] Path traversal em entrypoint/static rejeitado (`../` fora do worker dir).
- [x] `cargo test -p edger-isolation --features deno deno::cli` verde; mock ainda disponível com `--no-default-features` para CI rápido se necessário.
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
- [x] Criar bridge funcional `deno/cli.rs` para Deno CLI.
- [x] Roundtrip hello-world fetch via pipeline.
- [ ] Criar `deno_core` runtime: V8 platform init (singleton), op registration mínima, load module from path.
- [ ] Documentar sharp edges do spike em comentários/module docs.

### Fase 2 — ExecutionKind JS
- [x] `execute_fetch`: bridge `SerializedRequest`/`Response` JS ↔ Rust.
- [ ] `execute_routes`: convenção de export `routes` (objeto ou array) alinhada Buntime.
- [x] `serve_static_spa`: fs read + MIME + base injection.

### Fase 3 — Pool + limits
- [x] `DenoIsolate` wired no factory do orquestrador.
- [x] Bridge Deno CLI aplica timeout/process kill por `config.timeout_ms`.
- [x] Bridge Deno CLI executa no cwd do worker e carrega `deno.json`/`deno.jsonc`.
- [x] Bridge Deno CLI captura primeiro chunk de streams sem fim e cancela o reader.
- [x] Bridge Deno CLI captura Node `http.createServer` e adapta para Fetch Response.
- [x] Orquestrador injeta `x-base` e preserva base namespaced (`/@scope/app`) sem remover `@`.
- [x] `visibility: public` bypassa auth para worker inteiro.
- [ ] Supervisor: memory/CPU guard do backend embutido.
- [ ] `limits.rs`: port inicial de memory/CPU guard do spike.

### Fase 4 — Bundling + testes
- [ ] `bundle.rs`: suporte file-based v1; hook eszip se spike recomendou.
- [x] `workers/` real coberto em integration test (`hello-world`, `read-body`, `empty-response`, `serve-declarative-style`, `chunked-text`, `stream`, `sse`, `serve-html`).
- [x] Import map via `deno.json` coberto em integration test.
- [x] CommonJS server-listen simples coberto em integration test.
- [x] Paridade Buntime de path/base coberta em integration test namespaced.
- [x] Static SPA + manifest fallback via `package.json` cobertos em testes.
- [x] Validação manual cobre `logger-stdout` e `serve`.
- [ ] Gate workspace; atualizar README com feature flags.

## Verification
```bash
cargo test -p edger-isolation --features deno
cargo test -p edger-worker --features deno
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
