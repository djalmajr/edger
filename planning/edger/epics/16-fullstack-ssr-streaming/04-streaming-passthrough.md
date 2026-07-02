# Story 16.D: Streaming passthrough real (contrato Isolate + frames UDS)

**Origin:** `planning/edger/epics/16-fullstack-ssr-streaming/00-overview.md` (adiado do 15.E)

## Context

- **Problema:** o body de resposta é bufferizado (snapshot bounded). SSE nunca chega incremental ao cliente; SSR progressivo perde o benefício de TTFB. Era o adiado explícito do 15.E ("exige contrato `Isolate` streaming — grande blast radius").
- **Objetivo:** o worker streama o body por frames chunk/end no UDS; pool e orchestrator repassam os chunks ao cliente HTTP incrementalmente (axum `Body::from_stream`); SSE real observável com `curl -N`.
- **Valor:** SSE/streaming corretos de ponta a ponta; destrava SSR progressivo para os frameworks do epic.
- **Restrições:** caminho buffered continua o default do trait (blast radius controlado); limites preservados (byte cap/tempo viram guarda de sanidade, não truncamento de streams legítimos); cancel-safe (disconnect mid-stream não wedgeia worker).

## Traceability

- `planning/edger/epics/15-runtime-js-duravel/05-streaming-hardening.md` (adiados)
- `edger-isolation/src/multiproc.rs` (frames), `edger-isolation/src/multiproc_harness.mjs` (writer)
- `edger-worker/src/pool.rs` (`DispatchCancelGuard` — precisa acompanhar o stream)
- `edger-orchestrator/src/pipeline.rs` (conversão para axum Body)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/multiproc_harness.mjs` | edit | Header frame (status/headers) + chunk frames + end frame |
| `edger-isolation/src/multiproc.rs` | edit | `request_stream()`: lê header, expõe canal de chunks; frames tagueados |
| `edger-core/src/isolate.rs` | edit | Método streaming no trait com default buffered |
| `edger-core/src/wire.rs` | edit | Tipo de resposta streaming (status/headers + receiver de chunks) |
| `edger-worker/src/pool.rs` | edit | `fetch_stream`; guard/estado Active até end-frame ou drop do body |
| `edger-orchestrator/src/pipeline.rs` | edit | `Body::from_stream` quando o worker streama |
| `edger-isolation/tests/streaming.rs` | edit | Chunks chegam incrementais (timing); cancel mid-stream |
| `edger-orchestrator/tests/` | edit | E2E: SSE incremental de ponta a ponta |

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
- [ ] Worker `sse` emite eventos que chegam **incrementalmente** ao cliente (E2E mede chegada de ≥2 chunks em instantes distintos).
- [ ] Resposta não-streaming continua funcionando idêntica (paridade da matriz).
- [ ] Disconnect do cliente mid-stream: worker reciclado, sem wedge nem vazamento (teste).
- [ ] Erro do worker mid-stream fecha a resposta sem travar o cliente.
- [ ] Gates verdes (workspace + multiproc).

### Dependencies
- Story 15.E (leitura bounded atual, cancel guard)

## Tasks
### Fase 1 — Protocolo
- [ ] Frames tagueados no harness + `request_stream` no Rust; round-trip de chunks com timing.
### Fase 2 — Pool + pipeline
- [ ] `fetch_stream` com guard acompanhando o stream; axum `Body::from_stream`.
### Fase 3 — Prova
- [ ] E2E SSE incremental; cancel mid-stream; paridade da matriz; docs/compat-matrix (sse/stream → tested passthrough).

## Verification

```bash
cargo test -p edger-isolation --features multiproc --test streaming
cargo test --workspace
curl -N -H "Authorization: Bearer $KEY" http://127.0.0.1:3000/sse   # eventos pingando a ~1/s
```
