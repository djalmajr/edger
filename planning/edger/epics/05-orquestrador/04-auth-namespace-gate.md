# Story 05.04: Auth + namespace gate (ApiKeyPrincipal, Turso/SQLite, publicRoutes bypass)

**Origin:** `planning/edger/epics/05-orquestrador/00-overview.md`

## Context
- **Problema:** Requisições chegam ao pipeline sem autenticação nem verificação de namespace; contrato Buntime de principals não é aplicado.
- **Objetivo:** Gate early de auth com `ApiKeyPrincipal`, store Turso/SQLite e bypass de `publicRoutes`.
- **Valor:** Multi-tenancy seguro antes do dispatch; paridade com `planning/edger/design.md (auth/security; ai-memory zommehq/buntime)` e `planning/edger/design.md (ApiKeyPrincipal)`.
- **Restrições:** Persistência Turso/SQLite desde o início (decisão do usuário); root synthetic principal; gate antes de hooks e worker.

## Traceability
- **Source docs:** `planning/edger/design.md` (Auth, PR 7, Resolved Decisions — Turso imediato), Buntime `planning/edger/design.md (ApiKeyPrincipal)`
- **Design PR:** PR 7 — `feat(auth): principal resolution, namespace gating, root key, public route bypass`
- **Depende de:** Story 05.03, Epic 02 (`ApiKeyPrincipal`, `AuthProvider` trait)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-orchestrator/src/auth.rs` | criar | gate, root principal, namespace check |
| `edger-orchestrator/src/store.rs` | criar | Turso/SQLite API key store |
| `edger-orchestrator/src/pipeline.rs` | alterar | inserir gate antes de hooks/dispatch |
| `edger-core/src/auth.rs` | alterar (se necessário) | helpers `can_access_namespace` |
| `edger-orchestrator/tests/auth_gate.rs` | criar | cenários Buntime |
| `edger-orchestrator/Cargo.toml` | alterar | libsql/turso ou rusqlite |

## Detail

### AS-IS
`RequestContext.principal` sempre `None`; sem validação de API key.

### TO-BE
- `ApiKeyPrincipal` com campos: `id`, `namespaces` (`["*"]` ou `["@acme"]`), `role`, `permissions`
- Store:
  - Primário: Turso/libSQL (env `TURSO_DATABASE_URL` + token) ou SQLite local (`EDGER_AUTH_DB`)
  - Schema portado de Buntime (hash de key, metadata, namespaces)
- Gate no pipeline (ordem):
  1. Se rota em `publicRoutes` (relativa ou absoluta no manifest/global config) → skip auth + skip hooks de auth
  2. Extrair API key de header (`Authorization: Bearer` ou header Buntime equivalente)
  3. Root key (env `ROOT_API_KEY`) → synthetic principal com `namespaces: ["*"]`
  4. Lookup store → `Option<ApiKeyPrincipal>`
  5. Sem principal em rota protegida → 401
  6. `can_access_namespace(principal, worker.namespace)` → 403 se negado
- Preencher `RequestContext.principal` para downstream (hooks, logging)

### Escopo
- **In:** store, gate, root, publicRoutes, testes de todos cenários security wiki
- **Out:** implementação `AuthProvider` como extension crate (Epic 06 — `edger-ext-auth`); CSRF completo (Fase 7)

### Critérios de aceite
- [x] Root key acessa qualquer namespace
- [x] Key com `["@acme"]` acessa `/@acme/foo`, negado em `/@other/foo`
- [x] `publicRoutes: ["/health", "/login"]` bypassa gate (401 nunca)
- [x] Key inválida → 401; namespace negado → 403
- [x] Store persiste entre restarts (SQLite file test)
- [x] Testes não dependem de Turso cloud (SQLite local); Turso testado opcionalmente via feature flag

## Pendências
- Turso/libSQL atrás de feature `turso` não implementado (SQLite é primário em CI).
- Hashing SHA-256 com pepper fixo; migrar para argon2 quando portar schema Buntime completo.
- `edger-ext-auth` crate real permanece no Epic 06.

### Dependências
- Story 05.03 (pipeline)
- Epic 02: tipos auth

## Test-first plan
1. **Red:** request sem key em rota protegida → 401
2. **Red:** root key → 200 + principal `*`
3. **Red:** namespaced key wrong namespace → 403
4. **Red:** public route sem key → 200 (bypass)
5. **Red:** store insert + lookup roundtrip
6. **Green:** `auth.rs` + `store.rs`
7. **Refactor:** trait `ApiKeyStore` para mock em testes

**Nível:** integração (`auth_gate.rs`) + unit store

## Tasks
- [x] Definir trait `ApiKeyStore` + impl SQLite
- [ ] Impl Turso/libSQL atrás de feature `turso` (opcional CI) — pendência
- [x] Portar schema e hashing de keys (SHA-256 + pepper; argon2 pendente)
- [x] Implementar `authenticate(headers) -> Option<ApiKeyPrincipal>`
- [x] Implementar `is_public_route(path, config) -> bool`
- [x] Integrar gate em `pipeline.rs` antes de `HookRunner`
- [x] Root principal synthetic via env (`ROOT_API_KEY`)
- [x] Suite de testes: root, namespaced, public, invalid (6 testes)
- [x] Documentar env vars (`ROOT_API_KEY`, `EDGER_AUTH_DB`) no bin

## Verification
```bash
cargo test -p edger-orchestrator auth
cargo test -p edger-orchestrator
cargo clippy -p edger-orchestrator -- -D warnings
bun test
```