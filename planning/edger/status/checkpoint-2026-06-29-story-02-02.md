# Status: Story 02.02 — core models

**Mode:** Checkpoint  
**Story:** `planning/edger/epics/02-edger-core/02-core-models.md`

## Completed
- `manifest.rs`, `config.rs`, `principal.rs`, `execution.rs`, `worker_ref.rs`
- `parse_worker_config`, `infer_execution_kind`, Buntime YAML fixture tests (6 tests)

## Verification
- `cargo test -p edger-core models` — pass

## Pendência
- RoutesTable inference via module exports — deferred to orchestrator

## Next step
Story 02.03 — wire formats