# Closure: Story 11.04 vhosts e host routing

Date: 2026-07-01
Status: completed

## Files changed
- `edger-core/src/extension.rs`
- `edger-core/src/manifest.rs`
- `edger-ext-gateway/src/lib.rs`
- `edger-orchestrator/src/lib.rs`
- `edger-orchestrator/src/manifest_index_stub.rs`
- `edger-orchestrator/src/pipeline.rs`
- `edger-orchestrator/src/registry.rs`
- `edger-orchestrator/src/router.rs`
- `edger-orchestrator/tests/admin_workers_plugins.rs`
- `edger-orchestrator/tests/registry_providers.rs`
- `edger-orchestrator/tests/routing_resolution.rs`
- `edger-orchestrator/tests/value_parity.rs`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/11-gateway-operacional-avancado/00-overview.md`
- `planning/edger/epics/11-gateway-operacional-avancado/04-vhosts-host-routing.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/evidence/story-11-04-runtime.txt`

## Plan status
- [x] Define host routing rule and precedence with path routing.
- [x] Implement local resolution by `Host` header.
- [x] Add tests for hijack, namespace and fallback.
- [x] Update value matrix and docs with evidence.

## Behavior delivered
- Worker manifests now accept `hosts`.
- The manifest index stores normalized exact host aliases and rejects duplicate/unsafe aliases.
- Pipeline resolves known host aliases before shell fallback and after reserved path protection.
- Host-routed workers preserve namespace/auth behavior and receive `/` as effective base for vhost requests.
- Unknown hosts keep existing path routing behavior instead of matching a vhost.
- Gateway inventory declares `hostRouting` as an operational capability.

## Verification
See `planning/edger/status/evidence/story-11-04-runtime.txt`.

| Verification | Result |
|---|---|
| `cargo fmt -- --check` | passed |
| `cargo test -p edger-orchestrator --test routing_resolution` | passed |
| `cargo test -p edger-orchestrator --test value_parity` | passed |
| `cargo test -p edger-orchestrator --test admin_workers_plugins` | passed |
| `cargo test -p edger-orchestrator --test registry_providers` | passed |
| `cargo test -p edger-orchestrator --test shell_gateway` | passed |
| `cargo clippy --workspace -- -D warnings` | passed |
| `cargo test --workspace` | blocked by known sandbox TCP bind limitation in gateway proxy test |
| `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` | passed |

## Remaining risks
- Wildcard host routing, DNS, certificates and remote deploy routing remain out of scope.
- The full workspace test should be rerun on a host that allows binding the local proxy test socket.

## Next steps
- Continue the active roadmap sequence with Epic 07 work after 11.03.
