# Closure: Story 12.02 shell e catalogo de modulos

Date: 2026-07-01
Status: completed

## Files changed
- `edger-core/src/admin.rs`
- `edger-core/src/lib.rs`
- `edger-ext-gateway/src/lib.rs`
- `edger-orchestrator/src/admin_api.rs`
- `edger-orchestrator/tests/admin_workers_plugins.rs`
- `edger-orchestrator/tests/registry_providers.rs`
- `edger-orchestrator/tests/shell_gateway.rs`
- `workers/shell-demo/index.html`
- `workers/shell-demo/shell.js`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/roadmap.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/12-frontends-modulares-cpanel/00-overview.md`
- `planning/edger/epics/12-frontends-modulares-cpanel/02-shell-catalogo-modulos.md`
- `planning/edger/status/evidence/story-12-02-runtime.txt`

## Plan status
- [x] Define catalog shape derived from runtime capabilities.
- [x] Implement local shell/catalog worker.
- [x] Cover catalog auth, worker items, module menu contributions and shell routing in tests.
- [x] Update docs, roadmap, epic status and value matrix.

## Behavior delivered
- `GET /api/admin/catalog` exposes a root-only catalog contract for shell/frontends.
- Catalog worker items are derived from `ManifestIndex` admin worker inventory.
- Catalog module menu items are derived from extension `MenuContribution` capabilities.
- `edger-ext-gateway` now declares a `Gateway` menu contribution in its capability list and manifest inventory.
- `workers/shell-demo` renders the operational catalog, handles missing root credentials, treats disabled entries as non-actionable and keeps the key in memory only.
- Epic 12 is marked completed because cPanel, packaging, local Browser validation and shell/catalog v1 are now delivered.

## Verification
See `planning/edger/status/evidence/story-12-02-runtime.txt`.

| Verification | Result |
|---|---|
| `cargo fmt -- --check` | passed |
| `cargo test -p edger-orchestrator --test shell_gateway` | passed |
| `cargo test -p edger-orchestrator --test admin_workers_plugins` | passed |
| `cargo test -p edger-orchestrator --test registry_providers` | passed |
| `cargo clippy --workspace -- -D warnings` | passed |
| `cargo test --workspace` | blocked by known sandbox TCP bind limitation in gateway proxy test |
| `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` | passed |

## Remaining risks
- The full workspace test should be rerun on a host that allows binding the local proxy test socket.
- This story delivers shell/catalog v1, not a complete final admin UI or WebIDE.

## Next steps
- Continue with Story 11.04 vhosts/host routing.
