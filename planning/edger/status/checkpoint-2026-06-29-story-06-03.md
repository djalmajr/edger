# Checkpoint — Story 06.03 edger-ext-gateway template

**Data:** 2026-06-29  
**Story:** `epics/06-extensibilidade/03-extension-template.md`

## Entregue

- Crate `edger-ext-gateway` — `GatewayExtension` + `Middleware` pass-through
- `README.md` com passos copy-paste para nova extensão
- Testes: 3 unit + 1 integração (`gateway_integration.rs`)
- Bin `edger` registra `GatewayExtension::middleware()` (priority 0, após auth -100)
- `extensions.md` atualizado com diagrama de wiring

## Gates

- `cargo test -p edger-ext-gateway` verde
- `cargo test --workspace` verde
- `cargo clippy --workspace -D warnings` verde

## Pendências

| Item | Notas |
|---|---|
| `default-extensions` feature flag | Opcional na story; gateway sempre registrado em dev v1 |
| `scripts/new-extension.sh` | Fora de escopo; README basta |

## Próximo

Epic 06 closure → Fase 7 (Epic 07)