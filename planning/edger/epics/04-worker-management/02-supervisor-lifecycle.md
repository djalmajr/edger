# Story 04.02: Supervisor e lifecycle (Creating/Ready/Active/Idle/Terminating)

**Origin:** `planning/edger/epics/04-worker-management/00-overview.md`

## Context
- **Problema:** Workers sem máquina de estados supervisionada podem vazar memória, ignorar TTL ou não sinalizar READY antes de receber requests.
- **Objetivo:** Implementar `Supervisor` + estados em `WorkerInstance` conforme diagrama do design (espelho Buntime lifecycle).
- **Valor:** Confiabilidade operacional; base para health checks, retirement e integração com limites de isolation.
- **Restrições:** Estados em enum Rust; transições validadas (illegal transition → erro); READY exige validação mock de entrypoint.

## Traceability
- **Source docs:** `planning/edger/design.md` (Worker Lifecycle & Supervisor stateDiagram), PR 4
- **Depende de:** Story 04.01; Epic 03.02 (mock Isolate para spawn/ready); Epic 02.02 (WorkerConfig ttl_ms, idle_timeout)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-worker/src/supervisor.rs` | criar | `Supervisor`, transições, timers TTL |
| `edger-worker/src/instance.rs` | alterar | estado + dispatch + request count |
| `edger-worker/src/state.rs` | criar | `WorkerState` enum + `transition()` |
| `edger-worker/src/pool.rs` | alterar | integrar supervisor no get_or_create |
| `edger-worker/tests/supervisor_lifecycle.rs` | criar | transições válidas/inválidas |
| `edger-worker/Cargo.toml` | alterar | `tokio::time` para TTL timers |

## Detail

### AS-IS
- `WorkerInstance` skeleton sem estados
- Sem TTL sliding nem retirement

### TO-BE
- `WorkerState`: `Creating`, `Ready`, `Active`, `Idle`, `Terminating`, `Terminated`, `EphemeralTerm`
- Transições:
  - `Creating` → `Ready` após mock load + sinal READY
  - `Ready` → `Active` no primeiro dispatch
  - `Active` → `Idle` após response (inicia/reinicia timer TTL sliding)
  - `Idle` → `Active` em novo request (touch TTL)
  - `Idle` → `Terminating` quando TTL expira ou maxRequests atingido
  - `Active` → `Terminating` em erro crítico / health fail
  - `Terminating` → `Terminated` após cleanup + `isolate.terminate()`
  - `Ready` → `EphemeralTerm` quando ttl_ms=0 após response
- `Supervisor::spawn(instance)` — executa criação async, aplica ResourceLimits stub via callback
- `Supervisor::on_request_complete` — decrementa inflight, atualiza idle timer
- Health: flag `unhealthy` set em panic simulado ou erro Isolate

### Escopo
- **In:** state machine, supervisor, TTL timer (tokio), integrate pool
- **Out:** multi-process supervisor, cron, real health probes HTTP

### Critérios de aceite
- [x] Transição ilegal (ex: `Terminated` → `Active`) retorna erro
- [x] Worker ttl>0 permanece em pool após request e volta a `Idle`
- [x] Worker ttl=0 vai para `EphemeralTerm` e é removido do pool após cleanup
- [x] TTL expirado em `Idle` dispara `Terminating` → remoção LRU
- [x] `notify_idle` chamado no isolate ao entrar Idle (se trait disponível)
- [x] Testes cobrem caminho feliz e erro crítico

### Dependências
- Story 04.01
- Epic 03.02 (MockIsolate com terminate/notify_idle)

## Test-first plan
- **Primeiro teste falhando:** `creating_to_ready_requires_signal` — instance em Creating não aceita fetch até Ready
- **Nível:** `supervisor_lifecycle.rs` com `#[tokio::test]` e time control (`tokio::time::pause`)
- **Cenários:** TTL expiry com advance time, ephemeral ttl=0, critical error

## Tasks
- [x] Criar `state.rs` com enum + `transition(from, event) -> Result<WorkerState>`
- [x] Implementar `Supervisor` com spawn e timers
- [x] Enriquecer `WorkerInstance` com state, request_count, unhealthy flag
- [x] Integrar supervisor em `pool.fetch` (spawn on Creating)
- [x] Wire `on_request_complete` em `pool.fetch`
- [x] Testes de lifecycle + TTL + ephemeral (`supervisor_lifecycle.rs` — 7 tests)
- [x] Documentar mapeamento Buntime ↔ estados Rust (state.rs + design.md diagram)

## Pendências (documentadas)
- Timer TTL em background com `tokio::time::pause` flaky em multi-thread runtime — `on_ttl_expired` testado diretamente; timer E2E adiado para 04.04
- `get_or_create` permanece async; spawn explícito via `Supervisor::spawn` ou primeiro `fetch`
- Health probes HTTP reais — fora de escopo (04.02)

## Verification
```bash
cargo test -p edger-worker --test supervisor_lifecycle
cargo test -p edger-worker
cargo clippy -p edger-worker -- -D warnings
cargo fmt -- --check
bun test
```