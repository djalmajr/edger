# Story 04.04: Testes de integração do pool (mock isolate + fixtures tempfile)

**Origin:** `planning/edger/epics/04-worker-management/00-overview.md`

## Context
- **Problema:** Testes unitários isolados não garantem que pool + supervisor + métricas + mock isolate funcionam ponta-a-ponta como o orquestrador usará.
- **Objetivo:** Suite em `edger-worker/tests/` com fixtures de worker dir (manifest.yaml + entrypoint stub), `edger-isolation` mock como dev-dependency, cenários Buntime-like.
- **Valor:** Regressão forte antes do orquestrador (Fase 5); valida contratos de fetch/TTL/ephemeral juntos.
- **Restrições:** Testes usam `tempfile`; sem rede; dev-dep `edger-isolation` com feature mock; alinhar com disciplina ai-memory (`tests/` directory).

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 4, Migration notes TTL/ephemeral), `planning/edger/analysis-synthesis.md` (testes integração)
- **Depende de:** Stories 04.01, 04.02, 04.03; Epic 03.02 (`MockIsolate`); Epic 02.02 (manifest parse)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-worker/Cargo.toml` | alterar | `[dev-dependencies] edger-isolation`, `tempfile`, `serde_yaml` |
| `edger-worker/tests/integration_pool.rs` | criar | E2E pool + mock isolate |
| `edger-worker/tests/fixtures/` | criar | manifest.yaml samples (serverless, persistent, spa) |
| `edger-worker/tests/helpers/mod.rs` | criar | `setup_worker_dir`, `sample_request` |
| `edger-worker/src/factory.rs` | criar | `IsolateFactory` usando MockIsolate de edger-isolation |

## Detail

### AS-IS
- Testes fragmentados por módulo (pool_lru, supervisor, metrics)
- Sem fixture de worker dir realista
- Factory de isolate pode ser inline nos testes

### TO-BE
- `helpers/mod.rs`:
  - `fn temp_worker_dir(name, manifest: &str) -> TempDir` — escreve manifest.yaml + index stub marker
  - `fn serialized_get(path) -> SerializedRequest`
- `integration_pool.rs` cenários:
  1. **Persistent worker:** ttl=30s, dois fetch sequenciais → mesmo instance (hit), estado Idle entre requests
  2. **Ephemeral serverless:** ttl=0 → terminate após response, segundo fetch é miss
  3. **SPA static:** kind StaticSpa, mock retorna HTML com base href injetado
  4. **maxRequests:** manifest max_requests=1 → segundo fetch após retirement cria nova instance
  5. **Concurrent ephemeral:** dois fetch paralelos com concurrency=1 → um espera ou fila
  6. **Collision:** dois WorkerRef com mesma key → erro
  7. **Shutdown:** após shutdown, fetch retorna erro graceful
- `IsolateFactory` produz `MockIsolate::default()` configurável por teste
- Fixtures YAML espelham campos Buntime (entrypoint, ttl, maxRequests)

### Escopo
- **In:** integration tests, helpers, fixtures, factory wiring
- **Out:** testes com deno real, orquestrador HTTP

### Critérios de aceite
- [ ] `cargo test -p edger-worker --test integration_pool` — 7+ cenários passando
- [ ] Fixtures manifest parseiam via `edger-core::parse_worker_config`
- [ ] Dev-dep `edger-isolation` apenas em tests/factory (não em lib de produção se evitar acoplamento — factory trait object em worker, impl mock em test)
- [ ] Nenhum arquivo deixado em /tmp (tempfile RAII)
- [ ] Documentação no test module explica mapeamento Buntime

### Dependências
- Stories 04.01–04.03
- Epic 03.02 completo

## Test-first plan
- **Primeiro teste falhando:** `integration_persistent_worker_cache_hit` — dois fetch, assert metrics.cache_hits == 1
- **Nível:** `integration_pool.rs` only
- **Ordem de implementação:** helpers → factory → cenário persistent → demais cenários
- **Evitar:** Sleep real longo; usar `tokio::time::pause` para TTL

## Tasks
- [ ] Adicionar dev-deps ao Cargo.toml
- [ ] Criar fixtures YAML (serverless, persistent, spa)
- [ ] Criar `tests/helpers/mod.rs`
- [ ] Criar `factory.rs` com trait `IsolateFactory` + impl test
- [ ] Implementar cenários de integração 1–7
- [ ] Garantir pool usa factory injetada nos testes
- [ ] Atualizar `00-overview.md` epic status quando verde
- [ ] Rodar gate workspace completo

## Verification
```bash
cargo test -p edger-worker --test integration_pool
cargo test -p edger-worker
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
bun test
```