# Follow-up: recobrir testes de endpoints admin sobreviventes

**Origem:** Epic 17 (edger minimalista), commit de remoção do gateway + shell routing (2026-07-02).

## Contexto

Ao remover o `edger-ext-gateway` e o `shell_gateway`, deletei testes que estavam
entrelaçados com features removidas:

- `crates/edger-orchestrator/tests/gateway_integration.rs` — só gateway (ok deletar).
- `crates/edger-orchestrator/tests/registry_providers.rs` — providers de estado (serão removidos no epic).
- `crates/edger-orchestrator/tests/value_parity.rs` — paridade Buntime sobre features removidas.
- `crates/edger-orchestrator/tests/state_services.rs` — providers de estado.
- `crates/edger-orchestrator/tests/shell_gateway.rs`, `shell_routing_test.rs` — shell routing (removido).
- `crates/edger-orchestrator/tests/admin_workers_plugins.rs` — **entrelaçado demais** (114 refs de gateway); deletado inteiro.

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

## Entrega 2026-07-03

Coberto em `crates/edger-orchestrator/tests/admin_endpoints.rs`:

- Matriz de auth para leitura (`GET /api/admin/workers`) e mutação (`POST /api/admin/workers/{name}/disable`): sem key, key errada, root key e modo aberto.
- Rotas removidas de keys (`GET/POST /api/admin/keys`) permanecem fora com 404.
- Sessão admin retorna principal root (`role=admin`, `namespaces=["*"]`).
- Shape básico de `GET /api/admin/catalog` e `GET /api/admin/extensions`.
- Fluxo worker disable/enable afeta o data plane via `build_pipeline(...).oneshot(...)`, sem socket.
- Shape básico de `GET /api/admin/workers/error-summary` e `GET /api/admin/workers/{name}/errors`.
- Segurança CSRF para mutação admin com `Origin` cross-site retorna 403.

Não ficou débito de cobertura para os endpoints admin sobreviventes citados aqui. `install`/`rescan` continuam cobertos em `crates/edger-orchestrator/tests/deploy_install.rs`; esta entrega não recria teste profundo desses fluxos.
