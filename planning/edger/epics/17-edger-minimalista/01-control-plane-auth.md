# Story 17.A: Control-plane auth stateless (OIDC genérico + root-key)

**Origin:** `planning/edger/epics/17-edger-minimalista/00-overview.md`

## Context

- **Problema:** a auth hoje é uma extensão plugável com store SQLite de API keys (`edger-ext-auth`), pensada para autenticar o **data plane** (workers). Isso: (a) não persiste bem em K8s (in-memory some no restart; arquivo exige PVC/StatefulSet, que briga com HPA), e (b) é complexidade que, removida a auth do data plane, perde 95% da razão de existir.
- **Objetivo:** control plane (`/api/admin/*` + cPanel) protegido por auth **stateless e opt-in**, provider-agnóstica: um validador **OIDC genérico** (discovery + JWKS + claims padrão) e uma **root-key via Secret montado como arquivo** (hot-reload, sem restart) como break-glass. Data plane deixa de ter auth (story 17.B).
- **Valor:** resolve o problema de persistência de chaves em K8s de vez (sem store → nada a persistir → edger stateless/HPA-ready); desamarra de Keycloak (funciona com Okta/Auth0/Azure/Google ou nenhum); enxuga drasticamente.
- **Restrições:** OIDC é opt-in (sem `EDGER_OIDC_ISSUER` → desligado, roda sem IdP); nada de amarrar em um provider específico (só padrões OAuth/OIDC).

## Traceability

- `edger-ext-auth` (deletar), `edger-core/src/auth.rs` (`AuthProvider` trait — sai do registry), `edger-orchestrator/src/auth.rs` (`AuthGate`), `admin_api.rs` (`/api/admin/keys*` saem)

Decisão (2026-07-03): quando `EDGER_ROOT_KEY_FILE` e `ROOT_API_KEY` estiverem configurados, o arquivo (`EDGER_ROOT_KEY_FILE`) tem precedência sobre a env (`ROOT_API_KEY`).

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/control_auth.rs` | create | Middleware built-in: valida OIDC JWT (discovery/JWKS/claims) OU root-key (Secret-arquivo) OU aberto; só em `/api/admin/*` |
| `edger-orchestrator/src/admin_api.rs` | edit | Gatear com o novo middleware; remover `/api/admin/keys` e `/keys/{id}/revoke` |
| `edger-orchestrator/src/pipeline.rs` | edit | Trocar `AuthGate` do control plane pelo middleware novo |
| `edger-ext-auth/` | delete | Store SQLite de API keys deixa de existir |
| `edger-core/src/auth.rs` | edit | Remover `AuthProvider` trait (deixa de ser capability plugável) |
| `workers/cpanel/` | edit | Login simplifica para root-key; sem UI de gestão de chaves |
| `AGENTS.md`, `edger.rs` doc header | edit | Documentar `EDGER_OIDC_*` + `EDGER_ROOT_KEY_FILE` |

## Detail

### TO-BE
- **Middleware de control plane** (built-in, não extensão), aplicado só a `/api/admin/*`, resolve identidade por caminhos OR-ed, todos opt-in:
  1. **OIDC JWT** (se `EDGER_OIDC_ISSUER` setado): descobre `jwks_uri` via `.well-known/openid-configuration`, cacheia JWKS (rotação por `kid`), valida assinatura + `iss`/`aud`/`exp`/`nbf`; autoriza por role num claim configurável (`EDGER_OIDC_ROLES_CLAIM` + valor exigido) — agnóstico a Keycloak (`realm_access.roles`), Okta (`groups`), etc.
  2. **Root-key** (se `EDGER_ROOT_KEY_FILE` setado): bearer lido de um Secret **montado como arquivo**, relido no change (hot-reload → rotação sem restart). Break-glass / acesso de máquina (CI, curl).
  3. **Nenhum configurado** → control plane aberto (só dev).
- **Introspection (RFC 7662)** anotado como escape hatch futuro para providers de token opaco; v1 faz validação JWT local (cobre Keycloak e a maioria).
- Data plane não passa por aqui (17.B remove o `authorize` do worker).

### Scope
- **In:** middleware OIDC/JWT + root-key-arquivo; gate só no `/api/admin/*`; deletar `edger-ext-auth` e `AuthProvider`; remover endpoints de gestão de chaves; cPanel login simplificado; docs de env.
- **Out:** introspection de token opaco (escape hatch documentado); auth no data plane (removida em 17.B, não substituída).

### Acceptance criteria
- [ ] `EDGER_OIDC_ISSUER`+`AUDIENCE` setados → JWT válido de qualquer provider OIDC autentica `/api/admin/*`; JWT inválido/expirado/assinatura errada → 401.
- [ ] Claim de role configurável autoriza (Keycloak `realm_access.roles` e um genérico `groups` cobertos por teste).
- [x] `EDGER_ROOT_KEY_FILE` → bearer do arquivo autentica; **alterar o arquivo passa a valer sem restart** (hot-reload).
- [x] Nenhum configurado → `/api/admin/*` aberto (log de aviso).
- [x] `edger-ext-auth` deletado; sem store SQLite de chaves; `/api/admin/keys*` removidos; workspace compila.
- [x] cPanel loga com root-key sem fluxo de gestão de chaves.
- [ ] OIDC genérico (fase 2) pendente em 2026-07-03: discovery + JWKS + claims configuráveis via `EDGER_OIDC_*`.

### Dependencies
- Nenhuma (primeira story do epic).

## Tasks
### Fase 1 — Middleware
- [x] `auth.rs`: root-key via `ROOT_API_KEY` e `EDGER_ROOT_KEY_FILE` com hot-reload; modo aberto com warning quando nada configurado.
- [ ] OIDC genérico (fase 2) pendente em 2026-07-03: discovery + cache JWKS + verify JWT (assinatura/claims) via `EDGER_OIDC_*`.
### Fase 2 — Poda
- [x] Gatear `/api/admin/*`; remover endpoints de chaves; deletar `edger-ext-auth` + `AuthProvider`; simplificar cPanel.
### Fase 3 — Doc + prova
- [x] Testes da fase base: modo aberto, 401 com key errada/ausente, hot-reload da root-key e migração de testes para `ControlAuth`.
- [ ] Testes/docs OIDC (JWT válido/inválido, role por claim, `EDGER_OIDC_*`) e validação live no preview.

## Verification

```bash
cargo test -p edger-orchestrator
# live: JWT de um Keycloak/Okta de teste em /api/admin/*; editar o arquivo da root-key sem restart
```
