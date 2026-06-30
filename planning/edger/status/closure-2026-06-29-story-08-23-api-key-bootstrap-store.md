# Closure: Story 08.23 API key bootstrap store

Date: 2026-06-29
Status: completed

## Files changed
- `edger-ext-auth/tests/auth_provider.rs`
- `planning/edger/epics/08-valor-buntime/23-api-key-bootstrap-store.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/docs/compat-matrix.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`
- `planning/edger/status/evidence/story-08-23-runtime.txt`

## Plan status
- [x] Add focused auth bootstrap test.
- [x] Update value/compat docs and Epic 08 references.
- [x] Run full verification gates and capture evidence.

## Behavior delivered
- `SqliteApiKeyStore::open(path)` is now covered as a bootstrap-safe file-backed auth store.
- The focused test proves synthetic root auth, stored API key auth and auth after reopening the DB path.
- `API key bootstrap store` is now `tested` in the value matrix.
- Turso remoto/sync remains classified as Epic 09 external provider work, not an internal auth store requirement.

## Verification
See `planning/edger/status/evidence/story-08-23-runtime.txt`.
