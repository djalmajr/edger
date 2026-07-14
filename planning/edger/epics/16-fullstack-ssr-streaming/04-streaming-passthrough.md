# Story 16.D: Streaming passthrough real (contrato Isolate + frames UDS)

**Origin:** `planning/edger/epics/16-fullstack-ssr-streaming/00-overview.md` (adiado do 15.E)

## Context

- **Problema:** o body de resposta é bufferizado (snapshot bounded). SSE nunca chega incremental ao cliente; SSR progressivo perde o benefício de TTFB. Era o adiado explícito do 15.E ("exige contrato `Isolate` streaming — grande blast radius").
- **Objetivo:** o worker streama o body por frames chunk/end no UDS; pool e orchestrator repassam os chunks ao cliente HTTP incrementalmente (axum `Body::from_stream`); SSE real observável com `curl -N`.
- **Valor:** SSE/streaming corretos de ponta a ponta; destrava SSR progressivo para os frameworks do epic.
- **Restrições:** caminho buffered continua o default do trait (blast radius controlado); limites preservados (byte cap/tempo viram guarda de sanidade, não truncamento de streams legítimos); cancel-safe (disconnect mid-stream não wedgeia worker).

## Traceability

- `planning/edger/epics/15-runtime-js-duravel/05-streaming-hardening.md` (adiados)
- `crates/edger-isolation/src/multiproc.rs` (frames), `crates/edger-isolation/src/multiproc_harness.mjs` (writer)
- `crates/edger-worker/src/pool.rs` (`DispatchCancelGuard` — precisa acompanhar o stream)
- `crates/edger-orchestrator/src/pipeline.rs` (conversão para axum Body)

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/multiproc_harness.mjs` | edit | Header frame (status/headers) + chunk frames + end frame |
| `crates/edger-isolation/src/multiproc.rs` | edit | `request_stream()`: lê header, expõe canal de chunks; frames tagueados |
| `crates/edger-core/src/isolate.rs` | edit | Método streaming no trait com default buffered |
| `crates/edger-core/src/wire.rs` | edit | Tipo de resposta streaming (status/headers + receiver de chunks) |
| `crates/edger-worker/src/pool.rs` | edit | `fetch_stream`; guard/estado Active até end-frame ou drop do body |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | `Body::from_stream` quando o worker streama |
| `crates/edger-isolation/tests/streaming.rs` | edit | Chunks chegam incrementais (timing); cancel mid-stream |
| `crates/edger-orchestrator/tests/` | edit | E2E: SSE incremental de ponta a ponta |

## Detail

### TO-BE (desenho)
- **Protocolo UDS:** frames tagueados `[u32 len][u8 tag][payload]` — `tag=H` header JSON `{status, headers}`, `tag=C` chunk binário cru, `tag=E` end (com trailer opcional de erro). Harness e Rust mudam juntos (ship atômico; sem requisito de compat cruzada).
- **Harness:** em vez de drenar tudo, escreve header ao ter `Response`, e faz pump `reader.read() → chunk frame` até done → end frame. Guardas de sanidade (byte cap alto/tempo máximo generoso) só para runaway real.
- **Rust:** `DenoWorkerProcess::request_stream()` devolve `{status, headers, rx: mpsc::Receiver<Bytes>}` com task de leitura de frames; erro/EOF fecha canal.
- **Pool:** instance permanece `Active` enquanto o body streama; ao end-frame (ou drop do receiver = disconnect), transita para `Idle`/recicla. O `DispatchCancelGuard` move-se para dentro do wrapper do stream.
- **Orchestrator:** resposta axum com `Body::from_stream(ReceiverStream)`.

### Scope
- **In:** frames tagueados, request_stream, fetch_stream, pipeline streaming, SSE E2E incremental, cancel-safety mid-stream, atualização dos testes de 15.E afetados.
- **Out:** backpressure fino; HTTP/2 push; trailers; streaming de request body (upload) — resposta apenas.

### Acceptance criteria
- [x] Worker `sse` emite eventos que chegam **incrementalmente** ao cliente — E2E `streaming_e2e.rs::sse_events_reach_the_http_client_incrementally` mede 2 chunks em instantes distintos (~200ms), e ao vivo `curl -N /sse` recebe 1 evento/s.
- [x] Resposta não-streaming idêntica (paridade: `buffered_responses_unchanged_through_streaming_path` + suites express/hono/ssr/sveltekit/tanstack verdes).
- [x] Disconnect do cliente mid-stream: worker reciclado (`client_disconnect_mid_stream_recycles_the_worker`; `GuardedBody::drop` → terminate + evict), sem wedge.
- [x] Erro do worker mid-stream: frame `E` com erro → stream Rust encerra com `Err` → axum aborta o body sem travar; processo com stream anormal fica poisoned e o próximo request respawna.
- [x] Gates verdes (workspace + multiproc + clippy + fmt + oráculo).

### Dependencies
- Story 15.E (leitura bounded atual, cancel guard)

## Tasks
### Fase 1 — Protocolo
- [x] Harness escreve frames tagueados `H` (header JSON) / `C` (chunk cru, fatiado ≤1MB) / `E` (end, erro opcional); byte cap alto (`EDGER_STREAM_MAX_BYTES`, default 256MB) como guarda de runaway com truncamento limpo.
- [x] `DenoWorkerProcess`: socket split (read/write halves), `request_stream()` com pump task por request; read half volta por oneshot só em fim LIMPO (fim anormal = processo poisoned → próximo request falha rápido e o caller respawna). `request()` buffered reimplementado como collect do stream.
### Fase 2 — Contrato + pool + pipeline
- [x] `edger-core`: `WorkerResponse::{Buffered,Streamed}` + `BodyStream` (futures-core); trait `Isolate` ganha `execute_fetch_stream`/`execute_routes_stream` com **default buffered** (zero impacto nos demais isolates).
- [x] Pool: `fetch_worker_stream` — para fetch/routes streamable, os guards (dispatch lock + isolate lock owned) viajam DENTRO do body (`GuardedBody`): fim limpo → `on_request_complete` (Active→Idle) + métricas; drop/erro mid-stream → terminate + recycle. Efêmeros (ttl 0) permanecem buffered (permit com lifetime).
- [x] Pipeline: `fetch_worker_stream` + `streamed_to_axum` (`Body::from_stream`); hooks veem view headers-only em respostas streamed (mutação de status/headers propaga; body não é observável).
### Fase 3 — Prova
- [x] `streaming.rs::sse_stream_delivers_chunks_incrementally` (nível isolation, com poison assert); `streaming_e2e.rs` (3 E2E: incremental de ponta a ponta, disconnect recicla, paridade buffered); validação ao vivo no preview; compat-matrix sse/stream → tested passthrough.

## Verification

```bash
cargo test -p edger-isolation --features multiproc --test streaming
cargo test --workspace
curl -N -H "Authorization: Bearer $KEY" http://127.0.0.1:3000/sse   # eventos pingando a ~1/s
```

## Status

**completed** (2026-07-02) — Streaming passthrough real de ponta a ponta. O protocolo
UDS ganhou frames tagueados (`H` header / `C` chunk / `E` end): o harness escreve o
header assim que o handler retorna e bombeia os chunks conforme o worker os produz; no
Rust, `request_stream()` entrega status/headers imediatamente e os chunks fluem por um
pump task → mpsc → `BodyStream` → pool (`GuardedBody` carregando os locks de dispatch)
→ axum `Body::from_stream` → cliente. Semântica de falha explícita: fim de stream limpo
devolve o read half do socket (processo reutilizável, Active→Idle); disconnect do
cliente ou erro mid-stream desincroniza o socket por definição → processo poisoned,
instance reciclada, request seguinte ganha processo fresco. O trait `Isolate` ganhou
variantes `*_stream` com default buffered — Wasm/SPA/mock/v1 intocados; workers
efêmeros (ttl 0) seguem buffered. Provas: 4 testes de isolation (incremental com
timing + poison), 3 E2E de orquestrador (SSE incremental através do pipeline completo,
disconnect recicla, paridade buffered), suites de frameworks sem regressão, e validação
ao vivo no preview: `curl -N /sse` recebe **1 evento por segundo, incrementalmente**
(antes: snapshot único bounded após 2s) e `/stream` flui indefinidamente até o cliente
desconectar. Fora do escopo (mantidos): backpressure fino/HTTP2, trailers, streaming de
request body (upload).