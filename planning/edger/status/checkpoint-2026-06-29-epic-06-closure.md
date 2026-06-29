# Closure — Epic 06 Extensibilidade (Fase 6)

**Data:** 2026-06-29  
**Epic:** `epics/06-extensibilidade/00-overview.md`

## Stories entregues

| Story | Status | Evidência |
|---|---|---|
| 06.01 Registro estático | completed | `380f037`, `static_registration.rs`, `extensions.md` |
| 06.02 edger-ext-auth | completed | `edger-ext-auth/`, checkpoint story-06-02 |
| 06.03 Template gateway | completed | `edger-ext-gateway/`, checkpoint story-06-03 |

## Critérios de aceite do épico

- [x] Padrão de registro explícito no bin documentado
- [x] Regra choose ONE em `AGENTS.md` + `extensions.md`
- [x] `edger-ext-auth` implementa `AuthProvider`, testes verdes
- [x] Auth registrada no bin e pipeline autentica requests
- [x] `edger-ext-gateway` template Middleware com README
- [x] `cargo test --workspace && clippy -D warnings` verde
- [x] Extensões dependem apenas de `edger-core`

## Pendências cross-story (Epic 06)

| Item | Documento |
|---|---|
| inventory/linkme migration (3+ ext) | `01-static-registration.md` pendências |
| Turso auth store | `02-edger-ext-auth.md` → Epic 07 |
| Proxy/rate-limit real no gateway | `03-extension-template.md` out of scope |

## Próximo

**Fase 7 — Epic 07 Avançado.** Caminho crítico: stories 07.04 + 07.05 (execução real JS/Wasm) em paralelo.