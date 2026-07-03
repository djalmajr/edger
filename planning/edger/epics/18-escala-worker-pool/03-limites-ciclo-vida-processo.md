# Story 18.C: Limites e ciclo de vida por processo

**Origin:** `planning/edger/epics/18-escala-worker-pool/00-overview.md`

## Context

- **Problema:** com N processos por worker, cada processo precisa herdar corretamente os limites e lifecycle que hoje existem para uma única instância: heap cap por `lowMemory`, timeout, TTL deslizante, `idleTimeout`, `maxRequests`, erro crítico, OOM, streaming e shutdown.
- **Objetivo:** garantir que limites e reciclagem sejam por processo do grupo, com drain gracioso no shutdown e sem matar o grupo inteiro quando só uma instância atingiu limite.
- **Valor:** escala interna não pode trocar throughput por vazamento de processo, violação de limite ou perda de isolamento operacional.
- **Restrições:** continuar usando o design multiprocess do Epic 15; não reintroduzir `deno_core` embutido nem bridge Bun/CLI como fallback de escala.

## Traceability

- `edger-worker/src/supervisor.rs` (`on_request_complete`, `retire_for_max_requests`, `on_ttl_expired`, `on_critical_error`)
- `edger-worker/src/pool.rs` (`GuardedBody`, `complete_stream_state`, `recycle_stream_state`, `shutdown`)
- `edger-worker/src/instance.rs` (`request_count`, `ttl_handle`, `unhealthy`)
- `edger-isolation/src/limits.rs` (`ResourceLimits::from_config`)
- `edger-isolation/src/multiproc.rs` (`DenoWorkerProcess::spawn`, `DenoProcessIsolate::terminate`, V8 heap cap)
- `edger-isolation/tests/resource_limits.rs` (cap de heap mata só o worker que excede)
- `edger-worker/tests/supervisor_lifecycle.rs`, `edger-worker/tests/pool_error_recovery.rs`, `edger-worker/tests/cancel_safety.rs`

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-worker/src/supervisor.rs` | edit | Tornar lifecycle por instância compatível com grupo e drain |
| `edger-worker/src/pool.rs` | edit | Recycle/drain por processo; shutdown fecha fila e drena instâncias ativas |
| `edger-worker/src/instance.rs` | edit | Expor metadados de processo e estado de drain sem quebrar locks |
| `edger-isolation/src/multiproc.rs` | edit | Confirmar que cada processo criado pelo grupo recebe timeout/env/heap cap próprios |
| `edger-isolation/src/limits.rs` | edit | Manter `ResourceLimits::from_config` como fonte de limits por processo |
| `edger-worker/tests/supervisor_lifecycle.rs` | edit | Cobrir TTL/maxRequests por processo dentro de grupo |
| `edger-worker/tests/pool_error_recovery.rs` | edit | Provar que erro/OOM recicla só a instância afetada |
| `edger-isolation/tests/resource_limits.rs` | edit | Reusar cobertura de heap cap com múltiplos processos |

## Detail

### AS-IS
- `Supervisor::on_request_complete` incrementa `request_count`, aplica `max_requests`, chama `notify_idle` e agenda TTL para a instância.
- `DenoWorkerProcess::spawn` recebe `memory_mb` vindo de `ResourceLimits::from_config`.
- `DenoProcessIsolate` zera `self.process` quando request falha e `terminate` derruba o processo ao dropar.
- `WorkerPool::shutdown` hoje marca shutdown, limpa cache e métricas; com grupo/fila precisará impedir novos waiters e drenar ou encerrar processos existentes.

### TO-BE
- Cada processo do grupo tem seu próprio `request_count`, TTL handle, estado `Active/Idle/Terminating/Terminated` e heap cap.
- `maxRequests` recicla a instância atingida e, se houver demanda, o grupo repõe até manter capacidade configurada.
- TTL/idle expiram processo ocioso sem matar processos ativos do mesmo worker.
- Erro crítico/OOM recicla só a instância afetada; o grupo permanece saudável se outras instâncias estiverem boas.
- Shutdown fecha a fila, rejeita novos requests com erro de shutdown e deixa requests ativos terminarem até timeout de drain antes de terminar processos.

### Scope
- **In:** lifecycle por processo; drain em shutdown; recycle por `maxRequests`, TTL, erro, OOM; compat com streaming.
- **Out:** leader election/distributed drain; cgroups Kubernetes; métricas RSS/CPU reais por processo (podem ser follow-up de observabilidade).

### Acceptance criteria
- [ ] `ResourceLimits::from_config` é aplicado a cada `DenoWorkerProcess::spawn` criado pelo grupo.
- [ ] Worker com `maxProcesses: 2` e `maxRequests: 1` recicla uma instância por request sem encerrar o grupo inteiro.
- [ ] TTL expira somente processos idle; processo em streaming ativo não é morto pelo TTL.
- [ ] OOM/erro em uma instância não derruba processo vizinho do mesmo worker.
- [ ] Shutdown não aceita novos requests, não deixa fila pendurada e encerra processos após drain/timeout.
- [ ] Testes cobrem lifecycle por processo e falham se a implementação voltar a gerenciar lifecycle apenas no nível do worker inteiro.

### Dependencies
- Stories 18.A e 18.B

## Tasks
### Fase 1 — Estado por processo
- [ ] Identificar processo/instância dentro do grupo.
- [ ] Garantir `request_count` e TTL por instância.
- [ ] Proteger transições contra remoção concorrente.
### Fase 2 — Recycle e drain
- [ ] Reciclar instância por `maxRequests`, TTL, OOM e erro crítico.
- [ ] Repor capacidade conforme `minProcesses`/demanda.
- [ ] Implementar shutdown/drain da fila e dos processos.
### Fase 3 — Regressões
- [ ] Atualizar testes de supervisor.
- [ ] Adicionar teste de erro isolado em grupo.
- [ ] Adicionar teste de stream ativo durante TTL/shutdown.

## Verification

```bash
cargo test -p edger-worker supervisor_lifecycle pool_error_recovery cancel_safety
cargo test -p edger-isolation --test resource_limits --features multiproc -- --ignored --nocapture
cargo test --workspace
ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
# Durante requests ativos, encerrar o processo do servidor e confirmar logs/drain sem fila pendurada.
curl -s http://127.0.0.1:19080/metrics/stats
```

## Status

**completed** (2026-07-03) — lifecycle por processo e shutdown gracioso mergeados: limites aplicados por processo, reciclagem por instância, fila fechada em shutdown e drain de processos ativos antes da terminação.
