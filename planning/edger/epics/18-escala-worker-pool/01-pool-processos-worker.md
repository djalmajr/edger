# Story 18.A: Pool N-processos por worker

**Origin:** `planning/edger/epics/18-escala-worker-pool/00-overview.md`

## Context

- **Problema:** workers persistentes reutilizam módulo/processo, mas há uma única `WorkerInstance` por `WorkerCacheKey`. Concorrência no mesmo worker fica serializada no `dispatch_lock`, então uma réplica só atende 1 request por worker persistente por vez.
- **Objetivo:** permitir que um worker tenha N processos Deno persistentes dentro da mesma réplica, com configuração por manifesto, spawn sob demanda, `minProcesses` opcional, e roteamento least-busy/round-robin para uma instância disponível.
- **Valor:** aumenta throughput e reduz latência sob concorrência sem depender de HPA, sem reabrir estado no edger, e preservando o isolamento por processo entregue no Epic 15.
- **Restrições:** não usar bridge v1; não misturar workers no mesmo processo; não mover esse contrato para `edger-core` além de vocabulário/configuração pura.

## Traceability

- `edger-worker/src/pool.rs` (`WorkerPool::get_or_create`, `fetch_worker_inner`, `fetch_worker_stream_inner`)
- `edger-worker/src/instance.rs` (`WorkerInstance`, `dispatch_lock`, `isolate`)
- `edger-worker/src/lru.rs` (`WorkerLru`, cache atual de uma instância por chave)
- `edger-worker/src/types.rs` (`PoolConfig`, `WorkerCacheKey`)
- `edger-core/src/manifest.rs` (`WorkerManifest` sem campos de pool)
- `edger-core/src/config.rs` (`WorkerConfig`, `parse_worker_config`)
- `edger-isolation/src/multiproc.rs` (`DenoProcessIsolate`, `DenoWorkerProcess`)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/manifest.rs` | edit | Adicionar campos camelCase do manifesto: `concurrency`, `minProcesses`, `maxProcesses` |
| `edger-core/src/config.rs` | edit | Normalizar defaults e invariantes em `WorkerConfig` sem I/O |
| `edger-worker/src/types.rs` | edit | Estender `PoolConfig`/tipos de chave para suportar grupo de processos por worker |
| `edger-worker/src/instance.rs` | edit | Identificar instância/processo dentro do worker e expor estado suficiente para roteamento |
| `edger-worker/src/pool.rs` | edit | Trocar cache 1:1 por grupo por worker; spawn lazy; escolher instância least-busy/round-robin |
| `edger-worker/src/lru.rs` | edit | Preservar eviction por worker/processo com limites previsíveis |
| `edger-worker/tests/integration_pool.rs` | edit | Cobrir concorrência em worker persistente com N processos reais/fake isolates |
| `edger-worker/tests/pool_lru.rs` | edit | Garantir que eviction não remove grupo errado nem viola namespace/version |

## Detail

### AS-IS
- `WorkerPool::get_or_create` retorna uma `Arc<WorkerInstance>` por `WorkerCacheKey`.
- Cada `WorkerInstance` contém um único isolate e um único `dispatch_lock`.
- `DenoProcessIsolate` contém `process: Option<DenoWorkerProcess>`, criado lazy no primeiro dispatch e reutilizado.
- O manifesto aceita `ttl`, `timeout`, `idleTimeout`, `maxRequests` e `lowMemory`, mas não aceita configuração de concorrência por worker.

### TO-BE
- `WorkerManifest` ganha configuração explícita:
  - `concurrency`: atalho operacional para `maxProcesses` quando o usuário quer apenas "até N requests concorrentes".
  - `minProcesses`: processos pré-criados ou aquecidos após o primeiro resolve, default `0`.
  - `maxProcesses`: teto de processos persistentes por worker por réplica, default `1`.
- `parse_worker_config` normaliza invariantes: `maxProcesses >= 1`, `minProcesses <= maxProcesses`, `concurrency` não pode exceder `maxProcesses` quando ambos forem informados.
- `WorkerPool` passa a manter um grupo por `WorkerCacheKey`, com instâncias/processos individualmente rastreáveis.
- O roteamento escolhe instância livre por least-busy; em empate usa round-robin estável para evitar concentrar carga.
- Se todas estiverem ocupadas e o grupo ainda está abaixo de `maxProcesses`, cria nova instância sob demanda.
- `ttl`, `maxRequests` e erro crítico continuam sendo por processo/instância, não por grupo inteiro.

### Scope
- **In:** campos de manifesto/config; grupo de instâncias por worker; spawn lazy; `minProcesses` best-effort; roteamento least-busy/round-robin; testes de concorrência com isolates lentos.
- **Out:** HPA, métricas detalhadas por worker (18.D), fila/backpressure quando todos estão ocupados (18.B), Knative/FaaS, pre-warm no boot global.

### Acceptance criteria
- [ ] Worker com default sem campos novos mantém comportamento atual: 1 processo persistente por worker por réplica.
- [ ] Worker com `maxProcesses: 3` atende 3 requests concorrentes usando 3 instâncias/processos diferentes e não serializa no mesmo `dispatch_lock`.
- [ ] `minProcesses: 2` aquece até 2 instâncias sem ultrapassar `maxProcesses`; falha de aquecimento não derruba o worker inteiro se spawn sob demanda posterior puder recuperar.
- [ ] `maxRequests` recicla apenas o processo que atingiu o limite; o grupo continua atendendo com processos restantes ou respawn.
- [ ] Namespaces/versions continuam isolados: duas versões do mesmo worker não compartilham grupo nem processo.
- [ ] `cargo test -p edger-worker integration_pool pool_lru` cobre o novo comportamento com mutações claras: remover o fan-out ou ignorar `maxProcesses` deve quebrar os testes.

### Dependencies
- Nenhuma (primeira story do epic).

## Tasks
### Fase 1 — Contrato
- [ ] Adicionar campos de pool em `WorkerManifest`.
- [ ] Normalizar defaults/invariantes em `WorkerConfig`.
- [ ] Documentar que `concurrency` é atalho para capacidade por réplica, não HPA.
### Fase 2 — Estrutura do pool
- [ ] Introduzir grupo de instâncias por `WorkerCacheKey`.
- [ ] Manter ids/estado por processo para debug e métricas futuras.
- [ ] Ajustar eviction para remover processo/grupo sem misturar workers.
### Fase 3 — Roteamento e spawn
- [ ] Escolher instância livre por least-busy/round-robin.
- [ ] Criar instância sob demanda até `maxProcesses`.
- [ ] Garantir compat com `fetch_worker_stream_inner` e `GuardedBody`.
### Fase 4 — Testes
- [ ] Teste de concorrência com isolate lento provando paralelismo até N.
- [ ] Teste default 1 processo preservado.
- [ ] Teste namespace/version isolados.

## Verification

```bash
cargo test -p edger-core manifest config
cargo test -p edger-worker integration_pool pool_lru
cargo test --workspace
ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
# Em outro shell: disparar N requests concorrentes contra um worker lento e confirmar p95 menor com maxProcesses > 1.
curl -s http://127.0.0.1:19080/metrics | rg "edger_pool"
```

## Status

**implemented** (2026-07-03) — implementação local na branch `feat/worker-process-pool` adiciona configuração `concurrency`/`minProcesses`/`maxProcesses`, grupo de processos por worker, roteamento para instância livre com criação sob demanda até o teto, reciclagem por instância e testes dirigidos sem socket. Verificação executada com `CARGO_TARGET_DIR=/private/tmp/edger-pool-target`: `cargo check --workspace --all-targets`, `cargo fmt --all --check`, `cargo clippy -p edger-worker -p edger-orchestrator`, `cargo test -p edger-worker story18`, `cargo test -p edger-orchestrator --test pipeline_integration`.
