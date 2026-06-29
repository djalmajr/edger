# Checkpoint: Story 05.04 — Auth + namespace gate

**Date:** 2026-06-29  
**Story:** `epics/05-orquestrador/04-auth-namespace-gate.md`  
**Mode:** /agile-status checkpoint

## Progress
- `auth.rs` — `AuthGate`, `extract_api_key`, `is_public_route`, namespace gate
- `store.rs` — `ApiKeyStore` trait + `SqliteApiKeyStore`
- `pipeline.rs` — gate antes de dispatch; 401/403 mapping
- `tests/auth_gate.rs` — 6 cenários Buntime

## Gates
- `cargo test -p edger-orchestrator --test auth_gate`: 6 pass
- `cargo test -p edger-orchestrator`: 41 pass
- Evidence: SCRATCH/auth-gate-test.txt

## Next
- Story 05.05 extension registry