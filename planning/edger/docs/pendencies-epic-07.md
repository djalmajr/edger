# Pendências Epic 07 — Fase 7 Avançado

> HISTÓRICO desde o Epic 17: este arquivo registra pendências e decisões da fase
> pré-Epic 17. Itens sobre auth de worker, shell/gateway, bindings ou Turso não
> representam backlog atual do runtime minimalista.

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`  
**Atualizado:** 2026-07-02

Documento dedicado para itens não resolvidos durante execução da Fase 7.

## Bloqueadores cross-cutting

| ID | Item | Bloqueia | Destino |
|---|---|---|---|
| E07-B01 | deno_core V8 platform boot | 07.04 produção embutida | Story 03.04 carry-over; Deno CLI bridge cobre MVP funcional |
| E07-B02 | dispatch real JS/TS | exemplos JS/TS no runtime Rust | **Concluído v1** via Deno CLI bridge |
| E07-B03 | suíte compat JS movida para Rust | MVP funcional sem adapter Bun | **Concluído v1** em `kind_dispatch_integration.rs` |

## Por story

### 07.04 Real JS execution — **in progress (Deno CLI bridge v1)**

- [x] Adapter Bun removido; runtime ativo é Rust.
- [x] `DenoIsolate` executa `Deno.serve` e `export default { fetch }` via bridge Deno CLI.
- [x] `workers/hello-world`, `read-body`, `empty-response`, `serve-declarative-style`, `chunked-text`, `stream`, `sse`, `serve-html` passam pelo pipeline Rust em teste E2E.
- [x] `stream`/`sse` usam fallback bounded-first-chunk; passthrough streaming real segue pendente.
- [x] `deno.json`/import map local carregado no cwd do worker.
- [x] `logger-stdout` (import remoto) e `serve` (deno.json/JSR) responderam manualmente via bin Rust.
- [x] Adapter mínimo Node/server-listen cobre `commonjs`; `commonjs-hono` responde quando chamado pelo subpath interno `/commonjs-hono/commonjs-hono`, conforme semântica Buntime de path relativo + `x-base`.
- [x] `x-base` compatível com Buntime e base namespaced (`/@scope/app`) preservado no orquestrador.
- [x] Static SPA real serve `entrypoint: index.html`, assets, fallback e base injection.
- [x] `buntime/apps/todos` validado por HTTP sem `Authorization` e visualmente no Browser embutido do Codex.
- [x] Assets paralelos de SPA não falham mais por `WorkerPool` em estado `Active`; dispatches concorrentes do mesmo worker entram em fila.
- [x] Validação manual com `cargo run -p edger-orchestrator --bin edger` + `curl`.
- [x] Timeout/process kill por manifest no bridge Deno CLI.
- [ ] V8 singleton + op registration embutido (`deno_core` facade Edge Runtime) — aguarda aprovação explícita.
- [x] `execute_routes` production: bridge despacha `routes` export (exact > `:param` > `*`, method map 405, fallback `fetch`, 404 sem fallback); fixture `workers/routes-demo` + E2E em `kind_dispatch_integration.rs`.
- [x] `serve_static_spa` v1 com path traversal/base injection.
- [x] Harden de permissões/sandbox da Deno CLI bridge: migrado de `deno eval` (permissão total) para `deno run --no-prompt` com `--allow-read=<worker_dir>`, `--allow-env` sobre env limpo/filtrado e `--allow-net` configurável via `EDGER_DENO_ALLOW_NET` (`false|hosts`); write/run/ffi/sys negados. Testes `edger-isolation/tests/deno_sandbox.rs`.
- [x] Pool recicla worker após erro de isolate (antes ficava preso em `Active` e todo request seguinte falhava com `worker not ready for dispatch`); regressão em `edger-worker/tests/pool_error_recovery.rs`.

### 07.05 Wasm execution — **completed (standalone wasmtime v1)**

- [x] ABI mínima `http_status` + `http_body_len` + testes
- [x] Load from worker dir + pool E2E
- [x] Validação de módulo: magic bytes, tamanho máximo, imports host/WASI bloqueados
- [x] Env filter em `WasiConfig` (`AWS_*`, `DB_*`, `*_KEY`, `*_SECRET`)
- [x] `WorkerPool::fetch` usa `WorkerConfig.kind` quando `kind_hint` não é passado
- [x] Factory dinâmica do orquestrador Rust escolhe `WasmIsolate` por kind
- [x] Coexistência JS/TS + Wasm no mesmo processo/pool coberta por integração
- [x] Fixture `workers/wasm-hello/index.wat` documenta materialização opcional de `index.wasm`
- [ ] Host WASI real: preopen apenas worker root + env inject permitido
- [ ] ABI request/response em linear memory
- Ver `status/checkpoint-2026-06-29-story-07-05-wip.md`,
  `status/evidence/story-07-05-runtime.txt` e
  `status/closure-2026-07-01-story-07-05-wasm-execution.md`

### 07.01 Manifests + kinds — **completed**

- [x] `load_manifests_from_dirs` varre root/direct worker dirs e carrega `manifest.yaml`, `package.json` ou `index.*`
- [x] `RUNTIME_WORKER_DIRS` (`:`) integrado no bin Rust; default local `workers`
- [x] `enabled: false` ignorado; `latest` único resolve
- [x] Integração E2E por todos os `ExecutionKind`: fetch, routes, spa, wasm e fullstack (501) em `kind_dispatch_integration.rs` + `shell_routing_test.rs`

### 07.02 Shell routing — **completed**

- [x] Decisão de shell (document vs iframe, excludes, reserved paths) entregue na 08.05 (`shell_gateway.rs`)
- [x] SPA namespaced `/@scope/app` com `<base href>` injetado + asset relativo pela mesma rota (`shell_routing_test.rs`)
- [x] `injectBase: false` respeitado — fix em `edger-core::infer_execution_kind`, que fixava `inject_base: true` para `kind: spa` explícito
- [x] `planning/edger/docs/shell-protocol.md` com seção "Evolução planejada" (z-frame compat, WebTransport, `base_href`)

### 07.03 Cron nativo — **completed (scheduler v1)**

- [x] `CronScheduler` em `edger-orchestrator/src/cron.rs` registra jobs de
  workers habilitados e despacha requests internas pelo `Router` Axum local.
- [x] Requests internas usam `x-edger-internal: true`,
  `Authorization: Bearer $ROOT_API_KEY` e `x-request-id: cron-...`.
- [x] `workers/cron-worker` documenta manifest `cron[]` funcional.
- [x] `/metrics` expõe `edger_cron_executions_total` e
  `edger_cron_failures_total`.
- [x] Shutdown do binário cancela tasks cron antes do shutdown do pool.
- [ ] Full cron grammar/timezones/distributed leader election seguem fora do v1.

### 07.06 OTEL — **completed (observability v1)**

- [x] `edger-orchestrator/src/tracing_init.rs` centraliza startup de tracing,
  prefere `EDGER_LOG` sobre `RUST_LOG` e aceita `OTEL_EXPORTER_OTLP_ENDPOINT`
  e `OTEL_TRACES_SAMPLER` sem falhar startup.
- [x] Request sem `x-request-id` recebe UUID gerado antes do dispatch; response
  e worker observam o mesmo valor.
- [x] Dispatch de worker loga `request_id`, `worker_name`, versão e namespace
  sem headers de autenticação, API keys ou body.
- [x] `WorkerPool::fetch_worker` e o dispatch para isolate recebem spans leves
  com worker/kind para correlação.
- [x] `/metrics` inclui `edger_http_requests_total` e
  `edger_http_request_duration_ms_last`, além dos contadores de pool/cron já
  existentes.
- [x] Testes cobrem scrape Prometheus, propagação de request id gerado, logs
  redigidos e config de tracing/OTEL env.
- [ ] Export OTLP real segue pendente até linkar
  `tracing-opentelemetry`/exporter no workspace.

### 07.07 Hardening + compat matrix — **completed (v1)**

- [x] Ingress body cap retorna 413 antes de dispatch ao worker.
- [x] Header count >100 e header value >8KiB retornam 431 antes de dispatch ao
  worker.
- [x] `compat-matrix.md` tem smoke test para linhas críticas `tested` e partials
  conhecidos.
- [x] Harness `perf_harness` ignorado mede p50/p95 e hit rate de worker
  persistente in-memory.
- [x] `.github/workflows/ci.yml` roda Rust gate obrigatório e perf harness manual.
- [ ] Turso auth, argon2 keys (carry from 06.02)
- [ ] Perf scenarios slow/ephemeral/burst
- [ ] Per-worker body override no manifest
