# Pendências Epic 07 — Fase 7 Avançado

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`  
**Atualizado:** 2026-06-29

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
- [ ] V8 singleton + op registration embutido (`deno_core` facade Edge Runtime).
- [ ] `execute_routes` production específico.
- [x] `serve_static_spa` v1 com path traversal/base injection.
- [ ] Harden de permissões/sandbox/erros de filesystem da Deno CLI bridge.

### 07.05 Wasm execution — **in progress (v1)**

- [x] ABI mínima `http_status` + `http_body_len` + testes
- [x] Load from worker dir + pool E2E
- [x] Validação de módulo: magic bytes, tamanho máximo, imports host/WASI bloqueados
- [x] Env filter em `WasiConfig` (`AWS_*`, `DB_*`, `*_KEY`, `*_SECRET`)
- [x] `WorkerPool::fetch` usa `WorkerConfig.kind` quando `kind_hint` não é passado
- [x] Factory dinâmica do orquestrador Rust escolhe `WasmIsolate` por kind
- [ ] Host WASI real: preopen apenas worker root + env inject permitido
- [ ] ABI request/response em linear memory
- Ver `status/checkpoint-2026-06-29-story-07-05-wip.md`

### 07.01 Manifests + kinds — **in progress**

- [x] `load_manifests_from_dirs` varre root/direct worker dirs e carrega `manifest.yaml`, `package.json` ou `index.*`
- [x] `RUNTIME_WORKER_DIRS` (`:`) integrado no bin Rust; default local `workers`
- [x] `enabled: false` ignorado; `latest` único resolve
- [ ] Integração E2E por todos os `ExecutionKind` ainda depende 07.04 para JS real

### 07.02 Shell routing — **not started**

### 07.03 Cron nativo — **not started**

### 07.06 OTEL — **not started**

### 07.07 Hardening + compat matrix — **not started**

- Turso auth, argon2 keys (carry from 06.02)
- Harness performance baselines
