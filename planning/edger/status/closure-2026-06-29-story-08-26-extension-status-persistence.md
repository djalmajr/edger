# Closure: Story 08.26 persistência de status de extensões

Date: 2026-06-29
Status: completed

## Files changed
- `edger-orchestrator/src/registry.rs`
- `edger-orchestrator/src/bin/edger.rs`
- `edger-orchestrator/tests/admin_workers_plugins.rs`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/docs/extensions.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/epics/08-valor-buntime/26-extension-status-persistence.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`
- `planning/edger/status/evidence/story-08-26-runtime.txt`

## Plan status
- [x] Add optional JSON status store for extension enable/disable state.
- [x] Load status store from `EDGER_EXTENSION_STATUS_FILE` or `$EDGER_STATE_DIR/extension-status.json`.
- [x] Prove Admin API disable persists status and a rebuilt registry reads it.
- [x] Update value matrix, Epic 08 overview, roadmap, docs and checkpoint.

## Scope drift
- No drift outside the Story 08.26 acceptance criteria.
- The Admin API docs also corrected stale `501` wording for worker enable/disable because that was adjacent operational documentation and already contradicted delivered behavior.

## Behavior delivered
- `ExtensionRegistry` can attach an optional status store and persist known extension status as JSON.
- Admin API extension mutations now persist status when a status store is configured.
- Rebuilding the registry with the same status store preserves `disabled` inventory status.
- Dynamic reload/rescan and full manifest persistence remain explicit future gaps.

## Verification
See `planning/edger/status/evidence/story-08-26-runtime.txt`.

| Verification | Result |
|---|---|
| Targeted registry test | passed |
| Targeted Admin API test | passed |
| `cargo test --workspace` | passed |
| `cargo clippy --workspace -- -D warnings` | passed |
| `cargo fmt -- --check` | passed |
| `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` | passed |

## Remaining risks
- Extension discovery/reload remains explicit future work.
- Worker enable/disable still uses an in-memory overlay; this story only persists extension status.

## Next steps
- Continue reducing `must partial` rows in `planning/edger/docs/value-parity-matrix.md`.
