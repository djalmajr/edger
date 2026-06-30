# Closure: Story 08.09 API keys operacionais

Date: 2026-06-29
Status: completed

## Delivered

- Added core Admin API request/response vocabulary for API key creation and revocation.
- Extended `ApiKeyStore`, `AuthProvider` and `AuthGate` with create/revoke operations.
- Implemented SQLite-backed revocation in `edger-ext-auth`.
- Added root-only `POST /api/admin/keys` and `POST /api/admin/keys/{id}/revoke`.
- Kept raw API key exposure limited to the one-time create response.
- Updated value and compatibility matrices so API key creation/revocation is now tested.

## Explicit gaps

- Synthetic root key remains bootstrap-only and is not revocable through the store.
- Worker/plugin enable-disable was still a typed `501` when 08.09 closed; worker runtime toggle landed in 08.11 and extension runtime toggle landed in 08.13, with persistence still future.
- Audit log, rotation policy and remote key-store replication remain future work.

## Verification

```bash
cargo test -p edger-ext-auth
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
