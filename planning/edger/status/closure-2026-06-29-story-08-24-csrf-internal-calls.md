# Closure: Story 08.24 CSRF e chamadas internas

Date: 2026-06-29
Status: completed

## Files changed
- `edger-orchestrator/tests/security_operational.rs`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/epics/08-valor-buntime/24-csrf-internal-calls-contract.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/docs/compat-matrix.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`
- `planning/edger/status/evidence/story-08-24-runtime.txt`

## Plan status
- [x] Add focused non-root internal-header regression.
- [x] Update value/compat docs and Epic 08 references.
- [x] Run full verification gates and capture evidence.

## Behavior delivered
- Non-root API keys are not elevated by `x-edger-internal: true`.
- Current Admin API mutations have tested coverage for browser same-origin checks, CLI/API Bearer flows without `Origin`, root-only internal CSRF bypass and unauthenticated/non-root internal header denial.
- `CSRF e internal calls` is now `tested` in the value matrix for current admin mutation surfaces.

## Verification
See `planning/edger/status/evidence/story-08-24-runtime.txt`.
