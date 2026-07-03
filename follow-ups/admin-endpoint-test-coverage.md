# Follow-up: recobrir testes de endpoints admin sobreviventes

**Origem:** Epic 17 (edger minimalista), commit de remoção do gateway + shell routing (2026-07-02).

## Contexto

Ao remover o `edger-ext-gateway` e o `shell_gateway`, deletei testes que estavam
entrelaçados com features removidas:

- `edger-orchestrator/tests/gateway_integration.rs` — só gateway (ok deletar).
- `edger-orchestrator/tests/registry_providers.rs` — providers de estado (serão removidos no epic).
- `edger-orchestrator/tests/value_parity.rs` — paridade Buntime sobre features removidas.
- `edger-orchestrator/tests/state_services.rs` — providers de estado.
- `edger-orchestrator/tests/shell_gateway.rs`, `shell_routing_test.rs` — shell routing (removido).
- `edger-orchestrator/tests/admin_workers_plugins.rs` — **entrelaçado demais** (114 refs de gateway); deletado inteiro.

## Lacuna a recobrir

O `admin_workers_plugins.rs` também cobria endpoints admin **que sobrevivem**:

- Worker enable/disable (`/api/admin/workers/{name}/enable|disable`)
- Extension enable/disable + reconcile (podem sair no 17.E; recobrir só se sobreviverem)
- Catalog (`/api/admin/catalog`)
- Keys (mudam para root-only no 17.A — recobrir na forma nova)
- `require_root` / CSRF (admin mutation security — parte ainda coberta por `security_operational.rs`)

## Ação

Depois que o Epic 17 assentar (auth root-only, extensões possivelmente removidas),
escrever um `admin_api_test.rs` enxuto cobrindo os endpoints admin que **restarem**:
gate por root-key, worker enable/disable, catalog, e o que sobrar de extensões.
Não recriar cobertura de features deletadas.
