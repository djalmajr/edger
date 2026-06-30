# Story 08.11: Enable/disable runtime de workers

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz ainda marca `APIs de workers` como `partial` porque a Admin API lista workers, mas `enable/disable` responde `501`.
- **Objetivo:** Entregar enable/disable runtime, root-only e protegido por CSRF/internal-call guard, sem persistir manifesto em disco nesta fatia.
- **Valor:** Operadores conseguem retirar um worker de tráfego e reativá-lo sem reiniciar o processo nem editar arquivo manualmente.
- **Restrições:** A mutação é um overlay em memória; persistência segura, install/remove e hot reload completo continuam como futuras fatias.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `planning/edger/epics/08-valor-buntime/02-api-operacional-workers-e-plugins.md`, `planning/edger/epics/08-valor-buntime/03-seguranca-e-identidade-operacional.md`
- **Buntime refs:** worker lifecycle/control value from Buntime runtime and worker-pool docs referenced in Epic 08.
- **Prototype refs:** none; this is an API/operator workflow.
- **Business rules:** worker mutation is operational state and must not be available to non-root principals.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/admin.rs` | inspect | Reuse existing Admin mutation response vocabulary |
| `edger-orchestrator/src/manifest_index_stub.rs` | edit | Add in-memory enabled overlay and route filtering |
| `edger-orchestrator/src/admin_api.rs` | edit | Replace 501 worker mutation handlers with real enable/disable |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Cover admin mutation response and inventory status |
| `edger-orchestrator/tests/security_operational.rs` | edit | Update security tests from 501 to successful guarded mutation |
| `edger-orchestrator/tests/routing_resolution.rs` | edit | Cover disabled workers returning `NOT_FOUND` |
| `planning/edger/docs/value-parity-matrix.md` | edit | Update `APIs de workers` evidence |
| `planning/edger/docs/compat-matrix.md` | edit | Sync technical compatibility status |
| `planning/edger/status/evidence/story-08-11-runtime.txt` | create | Capture commands and results |

## Detail

### AS-IS
- `GET /api/admin/workers` returns worker inventory.
- Worker mutation routes are root-only and CSRF guarded, but return typed `501`.
- Route resolution does not have a runtime disabled overlay; manifests with `enabled: false` are skipped at load time.

### TO-BE
- `POST /api/admin/workers/{name}/disable` marks a loaded worker disabled in memory.
- `POST /api/admin/workers/{name}/enable` marks it enabled again.
- Inventory exposes status `disabled` or `loaded`.
- Dispatch to disabled workers returns `NOT_FOUND` before auth/worker execution.
- Non-root and cross-origin browser mutations remain denied.

### Scope
- **In:** root-only runtime enable/disable overlay, inventory status, routing behavior, tests and matrix/evidence.
- **Out:** install/remove, persisted manifest edits, hot reload of files, per-version mutation selection, multi-process replication.

### Approach
- Store enabled overrides inside `ManifestIndex`.
- Keep `ManifestIndex` internally synchronized so cloned `OrchestratorState` shares mutation state across requests.
- Route resolution ignores disabled entries when selecting workers, plugins, homepage and shell.
- Use existing `validate_admin_mutation_security` in Admin API.

### Risks
- **State sharing bug:** if mutation state is not shared across cloned router state, disable appears successful but dispatch still runs.
- **Overclaiming persistence:** docs must state runtime overlay only.
- **Security regression:** non-root or cross-origin mutation must not change state.

### Acceptance criteria
- [x] Root can disable and re-enable a worker through Admin API.
- [x] Inventory reflects disabled/enabled status.
- [x] Disabled worker dispatch returns `404`/`NOT_FOUND` without executing the worker.
- [x] Non-root and CSRF-denied mutation attempts do not disable the worker.
- [x] Matrices no longer claim worker enable/disable returns `501`.

## Test-first plan
- First failing test: `POST /api/admin/workers/todos/disable` with root returns success, subsequent inventory status is `disabled`, and `/todos` dispatch no longer reaches the worker.
- Preferred levels:
  - `admin_workers_plugins.rs` for API mutation contract and inventory status.
  - `routing_resolution.rs` for route resolution ignoring disabled workers.
  - `security_operational.rs` for mutation guard behavior with new success status.
- Low-value tests avoided: direct mutex implementation assertions or filesystem persistence checks.

## Tasks
- [x] Add runtime status mutation support to `ManifestIndex`.
  - Done when: disabled entries are hidden from route resolution and inventory reports status.
- [x] Replace worker mutation 501 handlers in Admin API.
  - Done when: root enable/disable returns typed success and missing worker returns typed error.
- [x] Add/update integration tests.
  - Done when: API, security and route tests cover enable, disable, inventory and denied mutation behavior.
- [x] Update matrices and evidence.
  - Done when: `APIs de workers` points at enable/disable evidence and persistence remains explicit out-of-scope.
- [x] Run focused and workspace gates.
  - Done when: focused tests, Rust gate and planning gate pass.

## Verification
```bash
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test -p edger-orchestrator --test security_operational
cargo test -p edger-orchestrator --test routing_resolution
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
