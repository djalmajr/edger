# Story 11.04: Vhosts e host routing

**Origin:** `planning/edger/epics/11-gateway-operacional-avancado/00-overview.md`

## Context

Buntime tem `plugin-vhosts` para roteamento por host. No edger, esse valor deve virar contrato de gateway que resolve apps por `Host` sem permitir hijack de rotas reservadas ou bypass de namespace.

**Depende de:** Story 11.01

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/src/lib.rs` | edit | Adicionar regras de host routing |
| `edger-orchestrator/src/router.rs` | edit | Integrar resolucao por host sem quebrar path routing |
| `edger-orchestrator/tests/routing_resolution.rs` | edit | Provar hosts, reserved paths e namespace |
| `edger-orchestrator/tests/value_parity.rs` | edit | Registrar fluxo representativo de vhost |

## Detail

### AS-IS

- Router resolve por path/base, namespace e versao.
- Gateway ainda nao usava `Host` para escolher app/plugin.

### TO-BE

- Regras de vhost mapeiam host exato para worker/app ou shell via `hosts` no manifesto.
- Reserved paths continuam protegidos independentemente do host.
- Host desconhecido segue comportamento explicito e testado.

### Scope

- **In:** contrato de host routing, testes locais com header `Host`, reserved paths.
- **Out:** DNS, certificados, wildcard publico, deploy remoto.

### Critérios de aceite

- [x] Host conhecido resolve o app esperado.
- [x] Host desconhecido retorna fallback seguro.
- [x] Reserved paths nao sao sequestrados por regra de vhost.
- [x] Namespace e semver continuam respeitados.

## Tasks

- [x] Definir regra de host routing e precedencia com path routing.
- [x] Implementar resolucao local por header `Host`.
- [x] Adicionar testes de hijack, namespace e fallback.
- [x] Atualizar matriz de valor com evidencia.

## Verification

```bash
cargo test -p edger-orchestrator --test routing_resolution
cargo test -p edger-orchestrator --test value_parity
cargo test -p edger-orchestrator --test admin_workers_plugins
cargo test -p edger-orchestrator --test registry_providers
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Status

completed (2026-07-01) - workers podem declarar `hosts` no manifesto para
roteamento por `Host` exato antes do shell fallback e depois da protecao de
paths reservados. O match normaliza lowercase/porta, rejeita aliases perigosos
e respeita enable/disable, namespace e versao do worker resolvido. Evidencia:
`planning/edger/status/evidence/story-11-04-runtime.txt` e
`planning/edger/status/closure-2026-07-01-story-11-04-vhosts-host-routing.md`.
