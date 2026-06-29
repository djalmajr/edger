# Checkpoint: Story 04.01 — WorkerPool + LRU

**Date:** 2026-06-29  
**Story:** `epics/04-worker-management/01-worker-pool-lru.md`  
**Mode:** /agile-status checkpoint

## Progress
- `edger-worker` crate implementado: `pool`, `lru`, `types`, `instance`, `factory`, `metrics`, `error`
- `WorkerPool::new` / `with_factory`, `get_or_create`, `fetch`, `shutdown`, `get_metrics`
- `IsolateFactory` trait para injeção (sem dep runtime em `edger-isolation`)
- LRU eviction com tracking de chaves evictadas (`WorkerError::Evicted`)
- 4 testes `pool_lru.rs` verdes

## Gates
- `cargo test -p edger-worker`: 4 pass
- `cargo test --workspace`: 35 Rust tests
- `cargo clippy --workspace -D warnings`: pass
- `bun test`: 6 pass

## Next
- Story 04.02 Supervisor lifecycle (`02-supervisor-lifecycle.md`)