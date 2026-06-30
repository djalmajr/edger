# Story 08.23: API key bootstrap store

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz ainda marca `API key bootstrap store` como `partial`, mesmo com `SqliteApiKeyStore` próprio. Falta evidência explícita de que autenticação operacional não depende do provider SQL durável nem de plugin state.
- **Objetivo:** Provar que root key e chaves persistidas funcionam antes do registro de providers duráveis, com store file-backed próprio do runtime.
- **Valor:** Operadores preservam acesso de bootstrap para instalar/gerenciar workers e extensões mesmo quando Turso remoto/sync ou providers de estado ainda não estão configurados.
- **Restrições:** Não copiar o desenho interno do Buntime, não mover auth para `DurableSqlProvider` e não tratar Turso remoto/sync como requisito interno do edger.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `planning/edger/docs/extensions.md`.
- **Buntime refs:** `wiki/data/storage-overview.md` no escopo `zommehq/buntime`, seção `API Keys Store`.
- **Prototype refs:** none; this is an operator/bootstrap contract.
- **Business rules:** root key is synthetic bootstrap state, not DB state; stored API keys must remain usable after process restart/reopen.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-auth/tests/auth_provider.rs` | edit | Add contract test for file-backed bootstrap auth independent of durable SQL provider |
| `planning/edger/docs/value-parity-matrix.md` | edit | Move `API key bootstrap store` from partial to tested with evidence |
| `planning/edger/docs/compat-matrix.md` | edit | Add explicit compatibility line for file-backed `EDGER_AUTH_DB` bootstrap |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | edit | Register Story 08.23 and update status/roadmap |
| `planning/edger/roadmap.md` | edit | Update Epic 8 story count/status |
| `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md` | edit | Record the new closed value slice |
| `planning/edger/status/closure-2026-06-29-story-08-23-api-key-bootstrap-store.md` | create | Closure report for the story |
| `planning/edger/status/evidence/story-08-23-runtime.txt` | create | Command evidence for focused and full gates |

## Detail

### AS-IS
- `AuthExtension::from_env()` can open `EDGER_AUTH_DB` or fall back to memory.
- `edger` registers auth before durable SQL/keyval/queue providers.
- The value matrix still describes the bootstrap store as partial because no evidence names this exact independence boundary.

### TO-BE
- `edger-ext-auth` has a focused test proving root key auth plus stored API key auth using a file-backed `SqliteApiKeyStore`.
- The test reopens the DB path and proves the stored key survives process-like restart boundaries.
- Planning docs classify the store as tested and keep Turso remoto/sync under Epic 09/provider boundaries.

### Scope
- **In:** file-backed auth store proof, root synthetic principal proof in the same contract, docs/matrix updates.
- **Out:** argon2 migration, root key rotation, remote/sync auth replication, OAuth/session UI, changing the store schema.

### Approach
- Keep `edger-core` as vocabulary/trait only.
- Avoid env mutation in the test; instantiate `SqliteApiKeyStore::open(path)` directly so the contract is deterministic.
- Prove the relevant operator behavior rather than rusqlite internals: root authenticates, stored key authenticates, reopen preserves key.

### Risks
- **Overclaiming:** SQLite file-backed auth is not Turso Sync. The docs must say remote/sync remains provider/external work, not an internal Epic 08 requirement.
- **False coupling:** The test must not import `DurableSqlProvider`, `edger-ext-turso` or orchestrator registry setup.

### Acceptance criteria
- [x] A focused auth test proves root key + persisted API key bootstrap without durable SQL provider.
- [x] `API key bootstrap store` is marked `tested` in the value matrix with current evidence.
- [x] Compatibility docs name `EDGER_AUTH_DB`/file-backed bootstrap explicitly.
- [x] Epic 08 overview, roadmap and checkpoint reflect Story 08.23.
- [x] Rust and planning gates are green.

## Test-first plan
- First failing test: `file_backed_store_bootstraps_auth_without_durable_sql_provider` in `edger-ext-auth/tests/auth_provider.rs`.
- Preferred level: crate integration test against real SQLite file via `SqliteApiKeyStore::open`.
- Observable behavior: a root key authenticates as synthetic root; a stored key authenticates; after reopening the DB path, the stored key still authenticates with same role, permissions and namespace.
- Low-value tests avoided: asserting SQLite schema internals, duplicating env parsing tests, or importing provider registry just to prove a negative.

## Tasks
- [x] Add focused auth bootstrap test.
  - Done when: `cargo test -p edger-ext-auth file_backed_store_bootstraps_auth_without_durable_sql_provider` passes.
- [x] Update value/compat docs and Epic 08 references.
  - Done when: matrix row is `tested`, Story 08.23 appears in backlog/status, and remote/sync remains Epic 09/provider-scoped.
- [x] Run full verification gates and capture evidence.
  - Done when: Rust gate, planning gate and diff check are recorded in `story-08-23-runtime.txt`.

## Verification
```bash
cargo test -p edger-ext-auth file_backed_store_bootstraps_auth_without_durable_sql_provider
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
git diff --check
```
