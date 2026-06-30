# Story 08.24: Contrato CSRF e chamadas internas

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz ainda marca `CSRF e internal calls` como `partial`, embora as mutações administrativas existentes já usem o guard de origem e o bypass interno autenticado. Falta fechar a evidência de que o header interno não autentica nem eleva uma API key não-root.
- **Objetivo:** Provar e documentar o contrato atual: browser-originated mutations exigem same-origin; chamadas internas só bypassam CSRF depois de autenticação root; clientes CLI autenticados continuam funcionando sem `Origin`.
- **Valor:** Operadores podem expor a Admin API para automações e futuros painéis sem transformar `x-edger-internal` em credencial pública.
- **Restrições:** Não introduzir OAuth/SSO, não criar upload/files API nesta fatia e não ampliar o escopo além das mutações admin já existentes.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `planning/edger/epics/08-valor-buntime/03-seguranca-e-identidade-operacional.md`, `docs/developers/06-operacao-e-testes.adoc`.
- **Buntime refs:** valor operacional de security/internal calls referenciado no Epic 08.
- **Prototype refs:** none; this is an API/operator security contract.
- **Business rules:** internal headers are control-plane hints, never credentials; mutating admin endpoints are root-only.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/tests/security_operational.rs` | edit | Add HTTP-level regression proving internal header does not elevate a non-root key |
| `planning/edger/docs/value-parity-matrix.md` | edit | Move `CSRF e internal calls` to tested for existing admin mutation surfaces |
| `planning/edger/docs/compat-matrix.md` | edit | Add technical compatibility line for CSRF/internal admin mutation contract |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | edit | Register Story 08.24 and update status |
| `planning/edger/roadmap.md` | edit | Update Epic 8 story count |
| `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md` | edit | Record the new closed value slice |
| `planning/edger/status/closure-2026-06-29-story-08-24-csrf-internal-calls.md` | create | Closure report for the story |
| `planning/edger/status/evidence/story-08-24-runtime.txt` | create | Command evidence for focused and full gates |

## Detail

### AS-IS
- `validate_admin_mutation_security` applies same-origin checks for browser-originated mutating requests.
- `x-edger-internal: true` bypasses CSRF only after root authentication.
- Existing tests cover missing/mismatched browser origin, same-origin, root internal bypass and unauthenticated internal header.
- The missing edge is explicit evidence that a valid non-root key is not elevated by the internal header.

### TO-BE
- `security_operational.rs` covers non-root bearer token plus `x-edger-internal: true` and expects `403`.
- The value matrix marks the existing CSRF/internal-calls contract as tested.
- Future file/upload/admin mutation surfaces remain required to reuse the same guard, but they are not counted as current gaps before they exist.

### Scope
- **In:** current Admin API mutations for keys, workers and extensions; CSRF same-origin; root-only internal bypass; non-root non-elevation.
- **Out:** OAuth, browser sessions, CPanel UI, file uploads, signed internal service tokens, audit logs.

### Approach
- Add one HTTP-level test through `build_pipeline(test_state())` instead of asserting only the guard helper.
- Keep the test on an existing worker mutation endpoint because it exercises auth, root requirement and mutation guard together.
- Update docs to describe the current contract and avoid claiming future upload surfaces are already implemented.

### Risks
- **Overclaiming:** `tested` applies to current Admin API mutation surfaces. New future mutation groups must add their own tests or reuse this guard before claiming coverage.
- **False security signal:** The internal header remains a bypass hint only after root auth; documentation must not describe it as a token.

### Acceptance criteria
- [x] Non-root key with `x-edger-internal: true` cannot mutate worker state.
- [x] Current CSRF/internal Admin API contract is documented as tested in the value matrix and compat matrix.
- [x] Story 08.24 is registered in Epic 08 overview, roadmap and checkpoint.
- [x] Rust and planning gates are green.

## Test-first plan
- First failing test: `internal_header_does_not_elevate_non_root_keys` in `edger-orchestrator/tests/security_operational.rs`.
- Preferred level: integration test through the real axum pipeline.
- Observable behavior: `POST /api/admin/workers/todos/disable` with `Bearer acme-read-token` and `x-edger-internal: true` returns `403` and does not mutate state.
- Low-value tests avoided: testing `header_is_true` directly, duplicating every admin endpoint, or adding a fake upload route just to make the matrix look complete.

## Tasks
- [x] Add focused non-root internal-header regression.
  - Done when: focused `security_operational` test passes.
- [x] Update value/compat docs and Epic 08 references.
  - Done when: `CSRF e internal calls` is `tested` for current admin mutation surfaces and future mutation groups remain explicitly scoped.
- [x] Run full verification gates and capture evidence.
  - Done when: Rust gate, planning gate and diff check are recorded in `story-08-24-runtime.txt`.

## Verification
```bash
cargo test -p edger-orchestrator --test security_operational internal_header_does_not_elevate_non_root_keys
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
git diff --check
```
