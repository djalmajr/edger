# Closure: Story 10.02 reconcile e reload controlado

Date: 2026-07-01
Status: completed

## Files changed
- `edger-core/src/admin.rs`
- `edger-core/src/lib.rs`
- `edger-orchestrator/src/admin_api.rs`
- `edger-orchestrator/src/registry.rs`
- `edger-orchestrator/tests/admin_workers_plugins.rs`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/10-operacao-extensoes-plugins/00-overview.md`
- `planning/edger/status/evidence/story-10-02-runtime.txt`

## Plan status
- [x] Define diff model between desired persisted extension status and effective in-memory registry status.
- [x] Add root-only Admin API reconcile endpoint.
- [x] Implement dry-run without side effects.
- [x] Apply runtime-supported enable/disable changes without dynamic loading.
- [x] Mark unknown desired extensions as `restartRequired`.
- [x] Preserve request ID and safe diagnostics.
- [x] Update docs, Epic 10 status and value parity evidence.

## Behavior delivered
- `POST /api/admin/extensions/reconcile` uses the persisted extension status document as desired state and the process registry overlay as effective state.
- `dryRun: true` returns the plan only.
- `dryRun: false` applies only registered extension enable/disable changes to the in-memory overlay.
- Desired extension names not present in the registry are returned as `restartRequired`; no crate hot-loading, remote download or deploy action is attempted.
- Reconcile responses include `requestId`, summary counts and diagnostics with no raw secrets or local status file path.

## Verification
See `planning/edger/status/evidence/story-10-02-runtime.txt`.

| Verification | Result |
|---|---|
| `cargo check -p edger-orchestrator --tests` | passed |
| Targeted reconcile tests | passed |
| `cargo test -p edger-orchestrator --test admin_workers_plugins` | passed |
| `cargo test --workspace` | blocked by known sandbox TCP bind limitation in gateway proxy test |
| `cargo clippy --workspace -- -D warnings` | passed |
| `cargo fmt -- --check` | passed |
| `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` | passed |

## Remaining risks
- Hot reload/rescan of extension crates remains out of scope by design.
- Validation of local extensions remains Story 10.04.
- The full workspace test should be rerun on a host that allows binding the local proxy test socket.

## Next steps
- Continue with Story 10.04 local extension validation.
