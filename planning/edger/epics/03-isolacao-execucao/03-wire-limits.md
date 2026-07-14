# Story 03.03: Wire handling, stubs de limites de recursos e prep multi-processo

**Origin:** `planning/edger/epics/03-isolacao-execucao/00-overview.md`  
**Status:** completed (2026-06-29)

## Context
- **Problema:** A fronteira isolate precisa validar wire types, aplicar limites (memória, CPU, timeout) e preparar transporte out-of-process sem acoplar ao orquestrador.
- **Objetivo:** Módulos `wire` e `limits` em `edger-isolation` com validação de `Serialized*`, stubs de resource limits, framing IPC preparatório.
- **Valor:** Multi-processo cedo; supervisor (Epic 04) aplica limites antes do dispatch.
- **Restrições:** Tipos wire canônicos em `edger-core`; limites são stubs funcionais (timeout real via tokio).

## Traceability
- **Source docs:** `planning/edger/design.md` (Serialized Request/Response, Multi-process decision)
- **Depende de:** Story 03.02; Epic 02.03; Epic 02.02

## Files
- `crates/edger-isolation/src/wire.rs`, `limits.rs`, `transport.rs`
- `crates/edger-isolation/tests/wire_limits.rs`, `limits_timeout.rs`

## Detail

### AS-IS
- Wire types existem em `edger-core` sem validação na camada isolation
- Sem framing IPC nem timeout wrapper

### TO-BE
- `validate_request` + postcard length-prefixed frames
- `execute_with_limits` com wall-clock timeout via tokio
- `InProcessTransport` default; `UdsTransport` stub para multiproc futuro

### Escopo
- **In:** validation, framing, limits stub, transport prep
- **Out:** UDS real, cpu_timer real, bincode multiproc

## Critérios de aceite
- [x] Request com 101 headers falha validação
- [x] Body acima de `max_body_size_bytes` rejeitado
- [x] Roundtrip encode/decode frame (postcard + length prefix)
- [x] `execute_with_limits` retorna Timeout quando mock sleep > limit
- [x] Documentação cpu_timer / base_mem_check em `limits.rs`
- [x] `cargo test -p edger-isolation` verde (12 tests)

### Pendências
- `parse_duration_string_to_ms("50ms")` no core falha (parser single-char unit) — testes usam `ResourceLimits` explícito; fix parser adiado para story core follow-up.
- `UdsTransport` real — feature `multiproc`, Epic 04+.

## Tasks
- [x] Implementar `wire.rs` validation + framing (postcard)
- [x] Implementar `limits.rs` com `ResourceLimits` e `execute_with_limits`
- [x] Implementar `transport.rs` com `InProcessTransport` + `UdsTransport` stub
- [x] Testes de validação, framing, timeout
- [x] Comentários multi-processo em `transport.rs`

## Verification
```bash
cargo test -p edger-isolation --test wire_limits
cargo test -p edger-isolation --test limits_timeout
cargo test -p edger-isolation
cargo clippy -p edger-isolation -- -D warnings
bun test
```