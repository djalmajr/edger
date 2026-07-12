# Checkpoint — Story 07.04 JS/TS real (Deno CLI bridge v1)

**Data:** 2026-06-29  
**Story:** `planning/edger/epics/07-avancado/04-real-js-execution.md`

## Entregue

- `edger-isolation/src/deno/cli.rs` — bridge Deno CLI por request.
- `DenoIsolate::execute_fetch` executa JS/TS real via `deno eval --no-check`.
- Captura de `Deno.serve(handler)` e `export default { fetch }`.
- Bridge JSON `SerializedRequest` → `Request` e `Response` → `SerializedResponse`.
- Execução com `current_dir` do worker e `deno.json`/`deno.jsonc` local quando presente.
- Streams infinitos/SSE têm fallback v1 bounded-first-chunk para evitar timeout no pipeline atual.
- CommonJS server-listen via adapter mínimo para `node:http.createServer`.
- Paridade Buntime para path montado: o worker recebe path relativo e header `x-base` com o base público.
- Static SPA real em Rust para `entrypoint: index.html`, com fallback para entrypoint e injeção de `<base href>`.
- `manifest.yaml` pode omitir `name`; loader usa `package.json` como fallback, como no app `todos`.
- `visibility: public` libera worker inteiro sem `Authorization`, necessário para abrir SPAs no browser.
- `WorkerPool` serializa dispatches concorrentes por instância; assets paralelos de SPA aguardam em fila em vez de falhar com `NotReady`.
- Timeout/process kill por `WorkerConfig.timeout_ms` (`DENO_TIMEOUT`).
- `edger-orchestrator` usa `DenoIsolate` para workers JS/TS e `WasmIsolate` para Wasm.
- Teste E2E contra `workers/` real cobre:
  - `hello-world`
  - `read-body`
  - `empty-response`
  - `serve-declarative-style`
  - `chunked-text`
  - `stream` (primeiro chunk)
  - `sse` (primeiro evento)
  - `serve-html`

## Evidência

- `cargo test -p edger-orchestrator --test kind_dispatch_integration` — 5 testes verdes.
- `deno_backend_loads_worker_deno_config_import_map` — `deno.json`/import map local validado.
- `deno_backend_times_out_hanging_streams` — timeout v1 validado.
- `cargo test --workspace` — verde.
- `cargo clippy --workspace -- -D warnings` — verde.
- `cargo fmt -- --check` — verde.

Validação manual:

```bash
ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
```

Responses observadas:

- `GET /ready` → `200 {"status":"ready"}`
- `GET /wasm-hello` → `200 wasm-hello`
- `POST /hello-world` com `{"name":"Alice"}` → `200 {"message":"Hello Alice from foo!"}`
- `POST /read-body` com `12345` → `200 {"totalSize":5}`
- `GET /empty-response` → `204`
- `GET /serve-declarative-style` → `200 Hello, world`
- `GET /chunked-text` → `200 meow`
- `GET /stream` → `200 Hello, World!\n` (bounded first chunk)
- `GET /sse` → `200 data: hella\r\n\r\n` (bounded first event)
- `GET /logger-stdout` → `200 {"hello":"world"}`
- `GET /serve` → `200 Hello, world`
- `GET /serve-html/foo` → `200` com `<h1>Foo</h1>`
- `GET /commonjs` → `200 Hello, World!`
- `GET /commonjs-hono` → `404 Not Found` por semântica Buntime: worker recebe `/`, mas o exemplo standalone registrou `/commonjs-hono`
- `GET /commonjs-hono/commonjs-hono` → `200 Hello, World!`

Validação real de SPA:

```bash
ROOT_API_KEY=test-root PORT=19084 RUNTIME_WORKER_DIRS=<buntime-repo>/apps/todos cargo run -p edger-orchestrator --bin edger
```

Sem `Authorization`:

- `GET /todos` → `200 text/html` com `<base href="/todos/" />`
- `HEAD /todos/index.css` → `200 text/css`
- `HEAD /todos/index.js` → `200 application/javascript`
- `HEAD /todos/favicon.ico` → `200 image/x-icon`
- `GET /todos/active` → `200 text/html` fallback SPA
- Browser embutido do Codex: `http://127.0.0.1:19084/todos` renderizou `TodoMVC`, removeu o loader, expôs input `What needs to be done?`, sem eventos de rede problemáticos e sem logs de console.

## Limites conhecidos

- Backend atual depende de `deno` no `PATH` ou `EDGER_DENO_BIN`.
- Execução é processo por request; funcional, mas não é o alvo final de performance.
- `stream`/`sse` ainda não são passthrough streaming real; o bridge retorna o primeiro chunk/evento e cancela o corpo.
- `deno_core` embutido segue pendente para produção.
- `execute_routes` específico ainda pendente.
- `commonjs-hono` expõe uma rota standalone com o próprio nome do worker. Em semântica Buntime, isso só casa quando chamado como subpath interno; para responder em `/commonjs-hono`, o worker deveria registrar `/`.
- Validação automatizada no Chrome ficou bloqueada nesta sessão; o Browser embutido do Codex validou a renderização visual da SPA.

## Próximo

Harden da bridge (permissões, sandbox e erros de filesystem), depois `deno_core` boot embutido sem quebrar a funcionalidade atual.
