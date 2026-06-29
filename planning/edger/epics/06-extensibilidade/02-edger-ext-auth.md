# Story 06.02: Primeira crate de extensão — edger-ext-auth (AuthProvider)

**Origin:** `planning/edger/epics/06-extensibilidade/00-overview.md`

## Context
- **Problema:** Lógica de auth vive no orchestrator; não há exemplo concreto de extensão Rust seguindo traits do core.
- **Objetivo:** Criar `edger-ext-auth` implementando `AuthProvider` com store Turso/SQLite e registro estático.
- **Valor:** Referência canônica para futuras extensões; auth desacoplado do orchestrator via OCP.
- **Restrições:** Depende apenas de `edger-core` (+ store deps); delega persistência portada da story 05.04.

## Traceability
- **Source docs:** `planning/edger/design.md` (AuthProvider trait, PR 9), Buntime `planning/edger/design.md (ApiKeyPrincipal)`, `planning/edger/design.md (auth/security; ai-memory zommehq/buntime)`
- **Design PR:** PR 9 — `feat: add edger-ext-auth as first extension`
- **Depende de:** Story 06.01, Epic 05 Story 05.04 (store/gate semantics), Epic 02 (`AuthProvider`, `ApiKeyPrincipal`)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-ext-auth/Cargo.toml` | criar | member workspace |
| `edger-ext-auth/src/lib.rs` | criar | `AuthExtension`, `AuthProvider` impl |
| `edger-ext-auth/src/store.rs` | criar | Turso/SQLite (mover ou compartilhar com orchestrator) |
| `edger-ext-auth/tests/auth_provider.rs` | criar | unit tests |
| `Cargo.toml` (workspace) | alterar | add member |
| `edger-orchestrator/src/bin/edger.rs` | alterar | register `AuthExtension` |
| `edger-orchestrator/src/auth.rs` | alterar | delegar ao AuthProvider do registry |
| `planning/edger/docs/extensions.md` | criar (nesta story) | exemplo auth |

## Detail

### AS-IS
Gate auth inline em `edger-orchestrator/src/auth.rs`; sem crate ext.

### TO-BE
- Crate `edger-ext-auth`:
  ```rust
  pub struct AuthExtension { store: Arc<dyn ApiKeyStore> }
  impl Extension for AuthExtension { ... }
  impl AuthProvider for AuthExtension {
      fn authenticate(&self, headers: &HeaderMap) -> Result<Option<ApiKeyPrincipal>>;
      fn can_access_namespace(&self, principal: &ApiKeyPrincipal, namespace: &str) -> bool;
  }
  ```
- `AuthExtension::new()` lê env (`EDGER_AUTH_DB`, `ROOT_API_KEY`, Turso vars)
- Registro via padrão 06.01
- Orchestrator: localizar extensão por nome `"auth"` no registry; pipeline chama trait em vez de lógica duplicada
- Middleware opcional: `on_request` pode rejeitar early (coordenar com gate — evitar dupla auth)

### Escopo
- **In:** crate completa, tests, wiring bin, refactor orchestrator para delegar
- **Out:** Rotação de keys UI, CSRF, OAuth (Fase 7)

### Critérios de aceite
- [x] `cargo test -p edger-ext-auth` verde
- [x] `edger-ext-auth` não depende de `edger-orchestrator` nem `edger-worker`
- [x] Bin registra auth e pipeline autentica requests protegidos
- [x] Paridade de testes com story 05.04 (root, namespace, public bypass)
- [x] Documentado em `extensions.md` como primeira extensão de referência

### Dependências
- Stories 06.01, 05.04
- Epic 02 traits

## Test-first plan
1. **Red:** `AuthExtension::authenticate` sem header → `None`
2. **Red:** root env key → principal `*`
3. **Red:** store seeded key → principal com namespaces corretos
4. **Red:** `can_access_namespace` deny/allow
5. **Green:** implementar crate
6. **Integração:** bin + registry + request protegido

**Nível:** unit (`auth_provider.rs`) + integração orchestrator

## Tasks
- [x] Scaffold `edger-ext-auth` no workspace
- [x] Extrair/mover `ApiKeyStore` trait para local compartilhado (core ou ext-auth)
- [x] Implementar `AuthExtension` + `AuthProvider`
- [x] Implementar registro estático (06.01)
- [x] Refatorar orchestrator `auth.rs` para delegar ao registry
- [x] Portar testes 05.04 para ext-auth onde aplicável
- [x] Atualizar `planning/edger/docs/extensions.md` com walkthrough edger-ext-auth
- [x] Verificar choose ONE: crate só auth, sem gateway

## Pendências (não bloqueantes)

| Item | Destino |
|---|---|
| Turso/libsql backend | Epic 07.07 ou ext dedicada |
| Argon2 para key hash | Epic 07.07 hardening |
| OAuth / CSRF | Fase 7 |

## Status
**completed** (2026-06-29) — checkpoint `status/checkpoint-2026-06-29-story-06-02.md`

## Verification
```bash
cargo test -p edger-ext-auth
cargo test -p edger-orchestrator auth
cargo test --workspace
cargo clippy --workspace -- -D warnings
bun test
```