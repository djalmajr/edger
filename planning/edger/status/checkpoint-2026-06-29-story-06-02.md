# Checkpoint — Story 06.02 edger-ext-auth

**Data:** 2026-06-29  
**Story:** `epics/06-extensibilidade/02-edger-ext-auth.md`

## Entregue

- Crate `edger-ext-auth` com `AuthExtension`, `SqliteApiKeyStore`, `AuthProvider` impl
- `ApiKeyStore` trait movido para `edger-core/src/api_key_store.rs`
- `extract_api_key_from_pairs` em `edger-core/src/auth.rs`
- Orchestrator: `store.rs` removido; `AuthGate` delega ao provider
- `ExtensionRegistry::register_auth_provider` para slot auth separado
- Bin `edger` registra `AuthExtension::from_env()`
- Testes: 4 unit (`auth_provider.rs`) + 6 integração (`auth_gate.rs`) — paridade 05.04

## Gates

- `cargo test --workspace` verde (~111 testes Rust)
- `cargo clippy --workspace -D warnings` verde
- `bun test` 6 pass

## Pendências documentadas

| Item | Notas |
|---|---|
| Turso/libsql | Apenas SQLite em v1; ver story 07.07 |
| Argon2 key hashing | SHA-256 + salt prefix em v1 |
| `can_access_namespace` no trait | Namespace check permanece em `AuthGate` via `principal_can_access_namespace` (core) |

## Próximo

Story 06.03 — template `edger-ext-gateway`