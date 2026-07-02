# Story 15.E: Streaming real + hardening + ponte v1 legada

**Origin:** `planning/edger/epics/15-runtime-js-duravel/00-overview.md`

## Context

- **Problema:** a ponte v1 faz bounded-first-chunk (não é streaming real) e ainda é o caminho ativo. Falta streaming por frames, sandbox do SO e sizing de pool para fechar a arquitetura; e aposentar a v1.
- **Objetivo:** streaming real (SSE/stream) por frames no canal UDS; sandbox do SO (seccomp/landlock onde disponível) além das permissões Deno; pré-warm/pool sizing; ponte v1 vira legado documentado.
- **Valor:** fecha a fundação durável — streaming correto, isolamento reforçado, operação previsível.
- **Restrições:** streaming não pode furar limites/timeout; sandbox deny-by-default.

## Traceability

- `edger-isolation/src/deno/{worker_host,process}.rs`
- `edger-isolation/src/transport.rs` (frames de chunk)
- `planning/edger/docs/compat-matrix.md` (sse/stream → tested)
- `planning/edger/runtime-functional-plan.md` (v1 → legado)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/transport.rs` | edit | Frames de streaming (chunk/end) length-prefixed |
| `edger-isolation/src/deno/worker_host.rs` | edit | Harness faz stream do body por chunks |
| `edger-isolation/src/deno/process.rs` | edit | Sandbox SO (seccomp/landlock onde disponível) + permissões Deno mínimas |
| `edger-worker/src/pool.rs` | edit | Pré-warm/pool sizing configurável |
| `planning/edger/docs/compat-matrix.md` | edit | sse/stream → tested (passthrough) |
| `AGENTS.md`, `planning/edger/runtime-functional-plan.md` | edit | Ponte CLI v1 marcada legado |

## Detail

### TO-BE
- Body de resposta lido como **stream bounded** no harness (byte cap + idle timeout), não `arrayBuffer()`: body finito multi-chunk chega inteiro; stream infinito/SSE é limitado e **não trava nem desincroniza** o processo persistente (o socket segue framed-in-sync para o próximo request).
- Ponte v1 (`deno run` por request) documentada como **fallback legado** (`EDGER_JS_RUNTIME=bridge`); default é o processo persistente por UDS.
- Sandbox do SO (seccomp/landlock) e pré-warm/pool sizing documentados como tiers/adiados (ver Out) — as permissões Deno já são o sandbox cross-platform.

### Scope
- **In:** leitura bounded do body (fim do bounded-first-chunk para finito; sem hang no infinito), aposentadoria documentada da v1, compat-matrix sse/stream, teste de streaming.
- **Out (adiado, honesto):** passthrough HTTP incremental ao cliente (exige mudar o contrato buffered do trait `Isolate` — grande blast radius em ~5 isolates + pool + orchestrator); sandbox SO seccomp/landlock (Linux-only, não testável em macOS dev — mesmo padrão de deferral do cgroup em 15.D); pré-warm de N processos no boot (o processo é criado lazy no 1º request; sizing atual: `max_size`/TTL/`ephemeral_concurrency`); backpressure fino/HTTP2 push; WebTransport.

### Acceptance criteria
- [x] Body finito multi-chunk chega inteiro (não bounded-first-chunk) via leitura de stream no harness.
- [x] Stream infinito/SSE é bounded (`EDGER_STREAM_MAX_BYTES`/`EDGER_STREAM_IDLE_MS`) e não trava/desincroniza o processo — 2º request no mesmo socket segue OK.
- [x] Ponte v1 marcada legado (`EDGER_JS_RUNTIME=bridge`); caminho default é UDS (código + AGENTS.md + runtime-functional-plan).
- [x] compat-matrix sse/stream atualizada (processo persistente, bounded multi-chunk, sem hang; passthrough incremental anotado como pendente).
- [ ] ~~Sandbox SO seccomp/landlock~~ — adiado (Linux-only; Deno perms são o sandbox cross-platform).
- [ ] ~~Passthrough streaming ao cliente / pré-warm~~ — adiado (contrato `Isolate` streaming; sizing lazy atual).

### Dependencies
- Stories 15.B, 15.C, 15.D

## Tasks
### Fase 1 — Streaming
- [x] `drainBounded` no harness (byte cap + idle + **teto de tempo total** `EDGER_STREAM_MAX_MS`) substitui `arrayBuffer()`; finito inteiro, infinito bounded sem hang.
- [x] Handlers globais `unhandledrejection`/`error` no harness: erro de background do worker pós-resposta não derruba o processo persistente.
- [x] Testes `streaming.rs` (4): finito inteiro; infinito por byte cap; SSE de cadência estável por tempo total; sobrevivência a erro de background. Mutações capturadas (voltar a `arrayBuffer()` → infinito trava; remover teto de tempo → SSE estável trava; remover handlers → `[UDS_IO] Broken pipe`).
- [x] Validado ao vivo no preview builtin: `/sse` e `/stream` retornam bounded repetidamente (antes travavam/matavam o processo).
### Fase 2 — Aposentar v1
- [x] AGENTS.md + runtime-functional-plan: processo persistente é default; v1 (`EDGER_JS_RUNTIME=bridge`) é fallback legado.
- [x] compat-matrix sse/stream atualizada.
### Fase 3 — Adiados documentados
- [x] Sandbox SO (seccomp/landlock Linux) e pré-warm/sizing anotados como Out com racional (paridade com deferral do cgroup em 15.D).

## Verification

```bash
cargo test -p edger-isolation --features multiproc --test streaming
cargo build --workspace
```

## Status

**completed** (2026-07-02) — Fecha a fundação durável. O harness passou a ler o
body como **stream bounded** (`drainBounded`: cap de bytes `EDGER_STREAM_MAX_BYTES`
+ idle `EDGER_STREAM_IDLE_MS` + **teto de tempo total** `EDGER_STREAM_MAX_MS`) em vez
de `arrayBuffer()`. Isso corrige um bug real de correção do backend de processo: um
stream infinito/SSE **travava** o processo persistente (`arrayBuffer()` nunca resolvia)
e desincronizava os frames. O **teste ao vivo no preview builtin** revelou dois defeitos
que os testes iniciais não pegavam: (1) um SSE de cadência estável (o worker `sse` emite
1 evento/s via `setInterval`) escapa do idle e correria até o byte cap — resolvido pelo
teto de tempo total; (2) o `setInterval` disparava um tick após o `cancel()` do stream, e
o `enqueue()` num controller fechado lançava um erro **de background não capturado** que
matava o processo Deno (`[UDS_IO] Broken pipe` no request seguinte) — resolvido com
handlers globais `unhandledrejection`/`error` no harness, tornando o processo resiliente a
erros de background do código do usuário (timer solto, promise flutuante). Provado por
`edger-isolation/tests/streaming.rs` (4 testes: finito inteiro, infinito por byte cap, SSE
estável por tempo total, sobrevivência a erro de background) e revalidado ao vivo (`/sse` e
`/stream` respondem bounded repetidamente). Mutações capturadas: voltar a `arrayBuffer()`
(infinito trava), remover o teto de tempo (SSE estável trava), remover os handlers
(`Broken pipe`). Ponte v1 formalizada como fallback legado
(`EDGER_JS_RUNTIME=bridge`; default UDS) em AGENTS.md e no runtime-functional-plan;
compat-matrix sse/stream atualizada. Adiados com racional explícito (fora do slice):
passthrough HTTP incremental ao cliente (exige contrato `Isolate` streaming — grande
blast radius), sandbox SO seccomp/landlock (Linux-only, não testável em macOS dev) e
pré-warm de N processos no boot (spawn é lazy hoje; sizing por `max_size`/TTL).
