# Story 03.03: Wire handling, stubs de limites de recursos e prep multi-processo

**Origin:** `planning/edger/epics/03-isolacao-execucao/00-overview.md`

## Context
- **Problema:** A fronteira isolate precisa validar wire types, aplicar limites (memória, CPU, timeout) e preparar transporte out-of-process sem acoplar ao orquestrador.
- **Objetivo:** Módulos `wire` e `limits` em `edger-isolation` com validação de `Serialized*`, stubs de resource limits inspirados em Edge Runtime, e framing IPC preparatório (postcard/bincode + length-prefix).
- **Valor:** Atende decisão do usuário de multi-processo cedo; supervisor (Epic 04) pode aplicar limites antes de delegar ao isolate.
- **Restrições:** Tipos wire canônicos permanecem em `edger-core`; isolation apenas valida/converte; limites são stubs funcionais (timeout real via tokio; memória/CPU como placeholders configuráveis).

## Traceability
- **Source docs:** `planning/edger/design.md` (Serialized Request/Response, Supervisor notes, Multi-process decision, Risks), PR 5
- **Depends on:** Story 03.02; Epic 02.03 (wire types); Epic 02.02 (WorkerConfig timeout_ms, max_body_size)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `crates/edger-isolation/src/wire.rs` | criar | validate/normalize Serialized*; encode/decode IPC frame |
| `crates/edger-isolation/src/limits.rs` | criar | `ResourceLimits`, `LimitGuard`, stubs mem/cpu |
| `crates/edger-isolation/src/transport.rs` | criar | trait `IsolateTransport`, `InProcessTransport`, `UdsTransport` stub |
| `crates/edger-isolation/Cargo.toml` | alterar | `postcard` ou `bincode`, `tokio` |
| `crates/edger-isolation/tests/wire_limits.rs` | criar | validação headers/body + roundtrip frame |
| `crates/edger-isolation/tests/limits_timeout.rs` | criar | timeout guard cancela execução mock lenta |
| `crates/edger-isolation/src/lib.rs` | alterar | exports |

## Detail

### AS-IS
- Wire types em `edger-core` sem validação na camada isolation
- Sem `ResourceLimits` nem transport prep
- Mock isolate não aplica timeout global

### TO-BE
- `validate_request(&SerializedRequest) -> Result<(), IsolationError>` — aplica constantes de header limits de core (100 headers, 64KiB total, 8KiB/value) + `max_body_size_bytes` de `WorkerConfig`
- `encode_frame` / `decode_frame` — length-prefixed postcard para futuro UDS/pipe
- `ResourceLimits { memory_mb, cpu_time_ms, wall_timeout_ms, low_memory }` com `LimitGuard` RAII
- `execute_with_limits(isolate, req, config, limits)` wrapper async que aplica `tokio::time::timeout` e retorna `IsolationError::Timeout`
- Stubs: `check_memory()` no-op ou contador simulado; `CpuTimer` struct vazia com TODO referenciando `cpu_timer` crate Edge Runtime
- `InProcessTransport` — chamada direta ao trait; `UdsTransport` — struct com métodos `connect`/`send` retornando `todo!` ou `NotImplemented` behind feature `multiproc`

### Escopo
- **In:** validação wire, framing, timeout wrapper, documentação multi-processo
- **Out:** processo filho real, supervisor remoto, integração pool (Epic 04)

### Critérios de aceite
- [ ] Request com 101 headers falha validação com erro tipado
- [ ] Body acima de `max_body_size_bytes` rejeitado antes de dispatch
- [ ] Roundtrip encode/decode frame preserva `SerializedRequest` com body binário
- [ ] `execute_with_limits` retorna Timeout quando mock sleep > `timeout_ms`
- [ ] Documentação em `limits.rs` referencia port futuro de `cpu_timer` / `base_mem_check`
- [ ] `cargo test -p edger-isolation` verde

### Dependências
- Story 03.02 (MockIsolate para testes de timeout)
- Epic 02.03 (wire constants/types)

## Test-first plan
- **Primeiro teste falhando:** `reject_oversized_header_value` — header 9KiB deve falhar
- **Nível:** `wire_limits.rs` (pure validation) + `limits_timeout.rs` (async)
- **Evitar:** Abrir sockets reais em CI; `UdsTransport` apenas compile test com `#[cfg(feature = "multiproc")]`

## Tasks
- [ ] Implementar `wire.rs` validation + framing (postcard)
- [ ] Implementar `limits.rs` com `ResourceLimits` e `execute_with_limits`
- [ ] Implementar `transport.rs` com traits e `InProcessTransport`
- [ ] Adicionar stub `UdsTransport` + feature flag `multiproc`
- [ ] Integrar wrapper de limites no dispatch de `kinds.rs` (opcional flag)
- [ ] Testes de validação, framing, timeout
- [ ] Comentários de arquitetura multi-processo (1 parágrafo em `transport.rs`)

## Verification
```bash
cargo test -p edger-isolation --test wire_limits
cargo test -p edger-isolation --test limits_timeout
cargo test -p edger-isolation
cargo clippy -p edger-isolation -- -D warnings
cargo fmt -- --check
bun test
```