# Closure: Story 10.04 validacao local de extensoes

Date: 2026-07-01
Status: completed

## Files changed
- `edger-orchestrator/tests/admin_workers_plugins.rs`
- `planning/edger/scripts/extension-validation.py`
- `planning/edger/scripts/run-gates.sh`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/roadmap.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/10-operacao-extensoes-plugins/00-overview.md`
- `planning/edger/epics/10-operacao-extensoes-plugins/04-validacao-local-extensoes.md`
- `planning/edger/status/evidence/extension-validation.txt`
- `planning/edger/status/evidence/story-10-04-runtime.txt`

## Plan status
- [x] Define local module validation entrypoint.
- [x] Validate manifest, inventory, status, diagnostics and redaction for a real extension.
- [x] Integrate validation into `run-gates.sh`.
- [x] Write versioned evidence under `planning/edger/status/evidence/`.
- [x] Document when to run local extension validation and the full Rust gate.
- [x] Mark Epic 10 completed in roadmap and overview.

## Behavior delivered
- `planning/edger/scripts/extension-validation.py --repo . --module gateway` runs a targeted Rust integration test without external network access.
- `run-gates.sh` now writes `extension-validation.txt` and fails if the local extension contract fails.
- The Rust test checks operational inventory shape, manifest config redaction, hook/provider requirements, status labels and sanitized gateway diagnostics.
- Epic 10 is marked completed because inventory, reconcile, manifest and local validation are now all delivered.

## Verification
See `planning/edger/status/evidence/story-10-04-runtime.txt`.

| Verification | Result |
|---|---|
| `python3 planning/edger/scripts/extension-validation.py --repo . --module gateway` | passed |
| `cargo test -p edger-orchestrator --test admin_workers_plugins` | passed |
| `cargo test --workspace` | blocked by known sandbox TCP bind limitation in gateway proxy test |
| `cargo clippy --workspace -- -D warnings` | passed |
| `cargo fmt -- --check` | passed |
| `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` | passed |

## Remaining risks
- Dynamic extension crate reload/rescan remains intentionally outside v1.
- The full workspace test should be rerun on a host that allows binding the local proxy test socket.

## Next steps
- Continue with Story 12.02 shell/catalogo de modulos.
