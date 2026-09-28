# Story 26.01: sessão root por senha

**Origin:** `planning/edger/epics/26-console-auth/00-overview.md`; autenticação do Apigate estudada em `.herdr-agents/wF/reports/scout-apigate-auth-20260927T130138.md`.

## Context

Hoje o EdgeR aceita root token, `egk_` e OIDC no control plane. O cPanel precisa de login por usuário e senha com sessão revogável, sem usar o token root como senha.

## Files

`crates/edger-orchestrator/src/console_auth.rs`, `auth.rs`, `admin_api.rs`, `bin/edger.rs`, `lib.rs`, `Cargo.toml`, `Cargo.lock` e testes sob `crates/edger-orchestrator/tests/`.

## Detail

`EDGER_ROOT_PASSWORD_FILE` aponta para senha inicial em arquivo. Sem conta `root`, ela é semeada com hash lento/salt aleatório, preservando operadores já criados pelo root token. Depois de criado, seed não substitui senha alterada. `POST /api/admin/login` emite sessão opaca aleatória `ses-` e persiste só o hash, com validade fixa de sete dias. `ControlAuth` aceita sessão via Bearer ou X-API-Key, mantendo root/egk/OIDC. `POST /api/admin/logout` revoga a sessão; `POST /api/admin/me/password` troca a senha e invalida sessões antigas. `GET /api/admin/login-options` informa disponibilidade sem segredos. Login limita tentativas por IP, valida Origin e não distingue usuário ausente de senha incorreta.

### Acceptance criteria

- Root é semeado apenas com arquivo válido; modo token funciona sem arquivo.
- Login correto entra em `/api/admin/session`, inválido falha genericamente, logout/expiração/troca negam sessão antiga.
- Banco indisponível falha fechado no login e root token permanece utilizável.

## Tasks

- [x] Implementar schema, seed, hash de senha e token de sessão sem texto claro no banco.
- [x] Integrar APIs e `ControlAuth` sem alterar autenticação de workers públicos.
- [x] Testar entradas hostis, rate limit, sessões e coexistência com credenciais existentes.

## Verification

```bash
cargo test -p edger-orchestrator --test console_auth
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

Registrar prova HTTP no binário local em `planning/edger/status/evidence/console-auth-2026-09-27.md`.
