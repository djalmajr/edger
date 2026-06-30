# Story 08.09: API keys operacionais

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz de valor ainda marca `API keys e sessão operacional` como `partial` porque o edger lista chaves e valida sessão, mas não cria nem revoga chaves por API.
- **Objetivo:** Entregar criação e revogação controladas de API keys pela Admin API, mantendo o segredo visível somente na resposta de criação.
- **Valor:** Operadores deixam de depender de bootstrap manual no store para conceder ou retirar acesso operacional.
- **Restrições:** Root synthetic key continua sendo bootstrap-only e não aparece na listagem; mutações devem passar pelo guard de segurança administrativa.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `planning/edger/epics/08-valor-buntime/02-api-operacional-workers-e-plugins.md`, `planning/edger/epics/08-valor-buntime/03-seguranca-e-identidade-operacional.md`
- **Buntime refs:** API key/session operational value from Buntime docs in the Epic 08 overview.
- **Prototype refs:** none; this is an API/operator workflow.
- **Business rules:** API keys are operational security state. Responses must not leak raw secrets except the one-time generated key on create.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/admin.rs` | edit | Add request/response vocabulary for key create/revoke |
| `edger-core/src/api_key_store.rs` | edit | Add revoke contract to the pure store trait |
| `edger-core/src/auth.rs` | edit | Expose create/revoke through the AuthProvider contract |
| `edger-ext-auth/src/lib.rs` | edit | Delegate create/revoke to the backing store |
| `edger-ext-auth/src/store.rs` | edit | Implement SQLite revoke and keep raw secret out of persisted metadata |
| `edger-ext-auth/tests/auth_provider.rs` | edit | Cover store/auth behavior after revoke |
| `edger-orchestrator/src/auth.rs` | edit | Expose create/revoke through AuthGate |
| `edger-orchestrator/src/admin_api.rs` | edit | Add protected admin key mutation endpoints |
| `edger-orchestrator/tests/admin_workers_plugins.rs` | edit | Add API-level create/revoke tests and leak checks |
| `planning/edger/docs/value-parity-matrix.md` | edit | Move API key operational status according to evidence |
| `planning/edger/status/evidence/story-08-09-runtime.txt` | create | Capture commands and results |

## Detail

### AS-IS
- `GET /api/admin/keys` lists key metadata without raw secrets.
- Non-root principals can read `/api/admin/session`.
- `SqliteApiKeyStore` supports `insert_key`, but the Admin API and AuthProvider do not expose create/revoke.

### TO-BE
- `POST /api/admin/keys` creates a key using a generated raw secret and returns that secret only once.
- `POST /api/admin/keys/{id}/revoke` removes a stored key and returns a typed mutation result.
- Both mutations require root credentials and pass `validate_admin_mutation_security`.
- A revoked key no longer authenticates and no list/session response leaks raw secrets.

### Scope
- **In:** generated API keys, SQLite-backed persistence, API-level create/revoke, tests, matrix/status evidence.
- **Out:** revoking the synthetic root key, UI, key rotation policy, audit log, rate limiting and remote key store replication.

### Approach
- Keep `edger-core` as vocabulary and traits only.
- Generate opaque keys in the orchestrator using UUID v4 and store only the existing salted hash plus prefix.
- Use `POST` mutation endpoints so the current same-origin/internal-request guard remains authoritative.
- Treat missing key revocation as idempotent operational behavior: return `revoked=false` rather than a misleading success with deleted state.

### Risks
- **Secret leakage:** only creation response may include `rawKey`; list/session/revoke responses must not.
- **Permission bypass:** non-root keys must not create/revoke other keys, even if they have `workers:read`.
- **CSRF regression:** browser-originated mutation with mismatched origin must fail before changing key state.

### Acceptance criteria
- [x] Root can create an API key through `POST /api/admin/keys`.
- [x] The created raw key is returned only in the create response and is not present in key list/session/revoke responses.
- [x] Created keys can authenticate for `/api/admin/session`.
- [x] Root can revoke a created key through `POST /api/admin/keys/{id}/revoke`.
- [x] Revoked keys no longer authenticate.
- [x] Non-root keys and mismatched browser origins cannot create or revoke keys.
- [x] `planning/edger/docs/value-parity-matrix.md` marks API key creation/revocation with current evidence.

## Test-first plan
- First failing test: `POST /api/admin/keys` with root credentials returns `201`, one-time `rawKey`, safe metadata, and the created key can call `/api/admin/session`.
- API contract tests:
  - non-root key cannot create or revoke;
  - mismatched browser origin cannot create;
  - revoked key no longer authenticates;
  - list response never contains raw generated key.
- Store test:
  - `revoke_key(id)` removes a stored key from lookup and reports whether a row changed.
- Low-value tests avoided: asserting UUID format beyond prefix, duplicating every field of serde JSON, or testing rusqlite internals.

## Tasks
- [x] Add core request/response structs and revoke trait method.
  - Done when: compile-time contracts represent create and revoke without I/O in `edger-core`.
- [x] Implement SQLite revoke and AuthProvider/AuthGate delegation.
  - Done when: store tests prove a revoked key cannot authenticate.
- [x] Add Admin API create/revoke endpoints.
  - Done when: root-only mutations use existing mutation security guard and never expose raw keys outside create.
- [x] Add integration tests for create, list leak prevention, non-root denial, CSRF denial and revoke.
  - Done when: `cargo test -p edger-orchestrator --test admin_workers_plugins` covers the new behavior.
- [x] Update value matrix and evidence.
  - Done when: `API keys e sessão operacional` no longer claims creation/revocation as a gap without evidence.

## Verification
```bash
cargo test -p edger-ext-auth
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
