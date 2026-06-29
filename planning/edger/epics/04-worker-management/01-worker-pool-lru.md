# Story 04.01: WorkerPool skeleton, LRU e get_or_create

**Origin:** `planning/edger/epics/04-worker-management/00-overview.md`

## Context
- **Problema:** Não existe pool de workers; cada request futuro precisaria spawn completo sem reuse, contrariando Buntime LRU + TTL.
- **Objetivo:** Implementar `WorkerPool` com LRU cache, método `get_or_create` (e `fetch` de alto nível), keyed por worker identity (dir + name + version).
- **Valor:** Base para orquestrador dispatch; reduz cold starts para workers persistentes (ttl > 0).
- **Restrições:** `edger-worker` depende só de `edger-core`; isolate criado via factory injetada; usar crate `lru` ou implementação interna documentada.

## Traceability
- **Source docs:** `planning/edger/design.md` (WorkerPool API, PR 4), Buntime wiki worker-pool (LRU, namespaces)
- **Depende de:** Epic 02.02 (WorkerRef, WorkerConfig); Epic 02.03 (SerializedRequest/Response para assinatura fetch)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-worker/Cargo.toml` | alterar | deps: `edger-core`, `tokio`, `lru`, `uuid`, `tracing` |
| `edger-worker/src/lib.rs` | criar/alterar | exports |
| `edger-worker/src/pool.rs` | criar | `WorkerPool`, `fetch`, `get_or_create` |
| `edger-worker/src/lru.rs` | criar | wrapper LRU + key `WorkerCacheKey` |
| `edger-worker/src/types.rs` | criar | `WorkerCacheKey`, `PoolConfig` |
| `edger-worker/src/instance.rs` | criar | `WorkerInstance` skeleton (sem supervisor completo) |
| `edger-worker/tests/pool_lru.rs` | criar | LRU hit/miss/eviction |

## Detail

### AS-IS
- Crate `edger-worker` vazio
- Sem cache nem identidade de worker em runtime

### TO-BE
- `PoolConfig { max_size, ephemeral_concurrency, ephemeral_queue_limit }` conforme design
- `WorkerCacheKey { worker_id: Uuid, name, version, dir_hash ou PathBuf }`
- `WorkerPool` interno: `LruCache<WorkerCacheKey, Arc<WorkerInstance>>` + métricas básicas (hits/misses)
- `get_or_create(worker_ref: &WorkerRef) -> Result<Arc<WorkerInstance>>` — miss cria instance em estado Creating (supervisor na 04.02)
- `fetch(dir, config, req, kind_hint) -> SerializedResponse` — resolve key, get_or_create, delega execução ao instance (mock)
- `shutdown()` — drena pool, terminate all
- Collision detection: mesmo `name` em dirs diferentes com namespace distinto permitido; conflito mesmo key → erro tipado `WorkerError::Collision`

### Escopo
- **In:** pool struct, LRU, keys, get_or_create, fetch skeleton, shutdown
- **Out:** supervisor state machine completo (04.02), métricas avançadas (04.03)

### Critérios de aceite
- [x] LRU evicta entrada menos recente quando `max_size` excedido
- [x] Segundo `fetch` para mesmo worker é cache hit (métrica hit++)
- [x] `get_or_create` com keys distintas para `@acme/app` vs `app` unscoped
- [x] `shutdown` esvazia pool e impede novos fetch (erro ou graceful)
- [x] `cargo test -p edger-worker --test pool_lru` verde
- [x] Assinatura `fetch` compatível com design.md

### Dependências
- Epic 02.02, 02.03
- Factory trait para isolate (definir em `edger-worker/src/factory.rs` ou usar callback)

## Test-first plan
- **Primeiro teste falhando:** `lru_evicts_oldest_when_full` — pool max_size=2, três workers, primeiro evictado
- **Nível:** `tests/pool_lru.rs` com mock isolate mínimo inline
- **Evitar:** Dependência runtime em edger-isolation no código de produção

## Tasks
- [x] Configurar `Cargo.toml` com workspace deps
- [x] Definir `WorkerCacheKey` + `PoolConfig` + `WorkerError`
- [x] Implementar LRU wrapper thread-safe (`Mutex<LruCache<...>>`)
- [x] Implementar `WorkerPool::new`, `get_or_create`, `fetch` (delega mock)
- [x] Implementar `shutdown`
- [x] Testes hit/miss/eviction/collision
- [x] Documentar injeção de isolate factory

## Pendências (documentadas)
- `dispatch_to_isolate` duplicado de `edger-isolation/kinds.rs` — candidato a mover para `edger-core` (evitar drift)
- Supervisor lifecycle (04.02) ainda não integrado em `WorkerInstance`
- `fetch` deriva nome do worker de `dir.file_name()` — orquestrador passará `WorkerRef` explícito na 04.04

## Verification
```bash
cargo test -p edger-worker --test pool_lru
cargo test -p edger-worker
cargo clippy -p edger-worker -- -D warnings
cargo fmt -- --check
bun test
```