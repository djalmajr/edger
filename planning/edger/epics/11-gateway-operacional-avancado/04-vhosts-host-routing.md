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
- Gateway ainda nao usa `Host` para escolher app/plugin.

### TO-BE

- Regras de vhost mapeiam host para worker/app, shell ou gateway rule permitida.
- Reserved paths continuam protegidos independentemente do host.
- Host desconhecido segue comportamento explicito e testado.

### Scope

- **In:** contrato de host routing, testes locais com header `Host`, reserved paths.
- **Out:** DNS, certificados, wildcard publico, deploy remoto.

### Critérios de aceite

- [ ] Host conhecido resolve o app esperado.
- [ ] Host desconhecido retorna fallback seguro.
- [ ] Reserved paths nao sao sequestrados por regra de vhost.
- [ ] Namespace e semver continuam respeitados.

## Tasks

- [ ] Definir regra de host routing e precedencia com path routing.
- [ ] Implementar resolucao local por header `Host`.
- [ ] Adicionar testes de hijack, namespace e fallback.
- [ ] Atualizar matriz de valor com evidencia.

## Verification

```bash
cargo test -p edger-orchestrator --test routing_resolution
cargo test -p edger-orchestrator --test value_parity
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

