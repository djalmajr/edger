# Story 15.A: Transporte UDS mínimo — worker Deno persistente + round-trip

**Origin:** `planning/edger/epics/15-runtime-js-duravel/00-overview.md`

## Context

- **Problema:** a ponte v1 spawna `deno eval` por request e captura por marcador de stdout; não há processo persistente nem protocolo binário. É a raiz dos ~40 ms/request.
- **Objetivo:** primeira fatia da arquitetura durável — o supervisor spawna **um processo `deno` persistente** rodando um harness que fala com o orquestrador por **postcard/UDS** (`UdsTransport`), e um request faz **round-trip** por esse canal, sem `deno eval`.
- **Valor:** prova o protocolo e o processo persistente end-to-end; base para módulo quente (15.B) e limites (15.D).
- **Restrições:** feature `multiproc`; reusar `wire::{encode_frame, decode_frame}` e `SerializedRequest/Response`; ponte v1 continua como caminho default até 15.B.

## Traceability

- `edger-isolation/src/transport.rs` (`IsolateTransport`, `UdsTransport` stub → real)
- `edger-isolation/src/wire.rs` (`encode_frame`/`decode_frame` postcard)
- `edger-core/src/wire.rs` (`SerializedRequest`/`SerializedResponse`)
- `edger-isolation/src/deno/` (harness JS reaproveitando a captura de handler da ponte v1)
- `edger-worker/src/supervisor.rs` (spawn/lifecycle)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/transport.rs` | edit | `UdsTransport` real: connect, envia frame req, lê frame resp (length-prefixed) |
| `edger-isolation/src/deno/worker_host.rs` | create | Geração do harness Deno persistente: conecta ao UDS, loop leia-request→dispatch→responda |
| `edger-isolation/src/deno/process.rs` | create | Spawn do processo `deno` persistente (socket path, env, sandbox flags v1) + handshake ready |
| `edger-isolation/src/deno/mod.rs` | edit | Backend que usa o processo persistente + `UdsTransport` sob `multiproc` |
| `edger-isolation/Cargo.toml` | edit | deps para UDS async (tokio net `UnixListener/UnixStream`) na feature `multiproc` |
| `edger-isolation/tests/uds_roundtrip.rs` | create | E2E: spawn worker Deno persistente, 1 request round-trip por UDS retorna resposta real |

## Detail

### AS-IS

- `UdsTransport` é stub (`connect` retorna erro "not implemented" / behind `multiproc`).
- Execução JS real só via `DenoCliRunner` (`deno eval` por request).

### TO-BE

- `WorkerHostProcess::spawn(socket_path)`: cria `UnixListener`, spawna `deno run harness.mjs` passando o socket path; espera handshake `ready`.
- Harness (JS): conecta ao UDS, importa o módulo do worker (uma vez já nesta fatia), e entra em loop: lê frame de request (length-prefixed JSON), despacha para o handler capturado (`Deno.serve`/default fetch), escreve frame de resposta (length-prefixed JSON).
- `UdsTransport` (Rust): `send(req) -> resp` escrevendo/lendo frames length-prefixed; erros tipados por I/O/timeout.
- Timeout por request e kill do processo no drop/erro crítico (reusar semântica do supervisor).

### Scope

- **In:** UDS transport real, processo Deno persistente, harness com round-trip, E2E de 1 worker.
- **Out:** integração no pool/orquestrador como default (15.B), limites de recurso (15.D), streaming real (15.E), frameworks (15.C).

### Acceptance criteria

- [x] `uds_roundtrip.rs` sobe um worker Deno persistente e faz round-trip por UDS retornando a resposta real (status/headers/body).
- [x] Nenhum `deno eval`/marcador de stdout; request/resposta trafegam como frame JSON length-prefixed (u32 LE + UTF-8).
- [x] Segundo request reusa o módulo já importado (contador de módulo prova: calls 1→2, sem re-spawn/re-import).
- [x] Falha de load retorna `IsolationError` tipado (`UDS_WORKER_FAILED`) com a causa; kill_on_drop encerra o processo; timeouts tipados (`UDS_TIMEOUT`).
- [x] Gate verde: workspace default + `--features multiproc` compila/testa; clippy multiproc `-D warnings`; fmt.

### Dependencies

- Story 07.04 (harness/captura de handler v1 como base), Epic 04 (supervisor)

## Test-first plan

- **Behavior:** E2E de crate exercendo o processo real (`deno` no PATH / `EDGER_DENO_BIN`) e o socket real; asserção sobre a resposta observável.
- **Level:** `edger-isolation/tests/uds_roundtrip.rs` (feature `multiproc`) + workspace gate.
- **Avoid:** mockar o transporte; o teste deve trafegar frames reais por UDS.

## Tasks

### Fase 1 — Transporte
- [x] Canal real `UnixStream` async, write/read de frames length-prefixed (u32 LE + JSON UTF-8) — `multiproc.rs`.
- [x] Erros tipados (`UDS_IO`, `UDS_TIMEOUT`, `UDS_BIND`, `UDS_SPAWN`, `UDS_WORKER_FAILED`).

### Fase 2 — Processo + harness
- [x] Spawn `deno run harness.mjs` com socket path + handshake ready (`DenoWorkerProcess::spawn`), sandbox `--allow-read=<worker_dir>` etc.
- [x] Harness JS (`multiproc_harness.mjs`): conecta, importa uma vez, loop request→dispatch→response com captura de `Deno.serve`/default fetch.

### Fase 3 — E2E
- [x] `uds_roundtrip.rs`: spawn + round-trip + 2º request reusa módulo + falha de load tipada.
- [x] Gate workspace + `--features multiproc`.


> **Nota de protocolo (2026-07-02):** o boundary Rust↔Deno usa **frames JSON
> length-prefixed** (u32 LE + JSON UTF-8), não postcard — o outro lado é
> JavaScript e implementar postcard em JS não se paga. O `encode_frame`/
> `decode_frame` postcard fica reservado para um futuro worker-em-Rust
> (mesma trait `IsolateTransport`). O corpo trafega como array de bytes,
> mesma convenção da ponte v1.

## Verification

```bash
cargo test -p edger-isolation --features multiproc --test uds_roundtrip -- --nocapture
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed** (2026-07-02) — `edger-isolation` feature `multiproc`:
`DenoWorkerProcess::spawn` sobe um processo `deno` persistente rodando
`multiproc_harness.mjs`, que importa o módulo do usuário **uma vez** e serve
requests round-trip por **UDS com frames JSON length-prefixed** (u32 LE + UTF-8),
substituindo o `deno eval` + marcador de stdout da ponte v1. Handshake ready,
sandbox por `--allow-read=<worker_dir>`, kill_on_drop, erros/timeouts tipados.
E2E `uds_roundtrip.rs`: módulo reusado (calls 1→2), falha de load tipada
(mutações provadas pelos comentários). **Latência do round-trip warm: p50 67us,
p95 490us** (vs. ~38 ms de spawn+re-import da v1 — ~600x). Evidência:
`status/evidence/js-runtime-perf-2026-07-02.md`.
