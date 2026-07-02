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
- Response body como sequência de frames de chunk até `end`; cliente monta o stream sem bufferizar tudo.
- Sandbox do SO no processo worker (deny-by-default), além de `--allow-*` mínimos do Deno.
- Pré-warm de N processos e reciclagem por idle/TTL; sizing configurável por env.
- Ponte v1 (`deno eval`) documentada como legado/fallback de emergência.

### Scope
- **In:** streaming por frames, sandbox SO, pré-warm/sizing, aposentadoria da v1, compat sse/stream.
- **Out:** backpressure fino/HTTP2 push; WebTransport (doc futura shell-protocol).

### Acceptance criteria
- [ ] `sse` e `stream` respondem em streaming real (múltiplos chunks), não bounded-first-chunk.
- [ ] Sandbox do SO ativo no worker (negativos: acesso fora do permitido falha).
- [ ] Pré-warm/pool sizing configurável; workers reciclam por idle/TTL.
- [ ] Ponte v1 marcada legado; caminho default é UDS.
- [ ] compat-matrix sse/stream → tested (passthrough).

### Dependencies
- Stories 15.B, 15.C, 15.D

## Tasks
### Fase 1 — Streaming
- [ ] Frames chunk/end no transporte + harness stream.
### Fase 2 — Sandbox + sizing
- [ ] Sandbox SO + permissões mínimas; pré-warm/sizing.
### Fase 3 — Aposentar v1
- [ ] Docs (AGENTS/plan) v1 legado; compat-matrix atualizada.

## Verification

```bash
cargo test -p edger-orchestrator --test kind_dispatch_integration --features multiproc -- sse stream
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
