# Closure — Story 08.03 Segurança operacional

**Data:** 2026-06-29  
**Story:** `planning/edger/epics/08-valor-buntime/03-seguranca-e-identidade-operacional.md`  
**Epic:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Resultado

Story 08.03 concluída como segurança operacional v1. O edger agora aplica inventário de workers por namespace/permissão, CSRF same-origin para mutações administrativas browser-facing, bypass interno somente depois de autenticação root, request ID em erros admin e limites HTTP tipados antes do dispatch.

## Entregue

- `edger-core/src/security.rs` com helpers puros para header interno, métodos mutáveis, same-origin, permissões, namespace opcional e env sensível.
- `edger-orchestrator/src/security.rs` com guard HTTP para CSRF/internal-call em rotas administrativas.
- `GET /api/admin/workers` agora aceita root ou key com `workers:read`; keys não-root veem apenas workers dentro dos seus namespaces.
- Mutações administrativas continuam root-only e protegidas por auth/CSRF. Nesta story elas ainda retornavam `501`; 08.11 e 08.13 substituíram os stubs de worker/extensão por toggles runtime reais em memória, enquanto persistência segue futura.
- Limites de ingresso retornam `413 PAYLOAD_TOO_LARGE` para corpo excedido e `431 HEADER_TOO_LARGE` para headers excedidos, sem iniciar worker.
- WASI reusa `edger_core::is_sensitive_env_key` e cobre padrões como `DATABASE_URL`, `OPENAI_API_KEY`, `_TOKEN`, `_SECRET` e `_PASSWORD`.
- `docs/developers/06-operacao-e-testes.adoc` documenta namespace, CSRF/internal calls e limites.
- `planning/edger/docs/value-parity-matrix.md` aponta evidências da 08.03 e mantém lacunas futuras explícitas.

## Drift de escopo

- CSRF v1 protege browser-originated mutations (`Origin` ou `Sec-Fetch-*`) sem quebrar clientes CLI/API com Bearer token e sem `Origin`.
- Listagem namespace-aware foi aplicada a workers. Plugins, files, uploads e criação/revogação de keys ainda não existem como superfícies completas no edger.
- Mutações de worker permanecem root-only por segurança; autorização namespace-aware de mutações reais deve esperar persistência segura de manifest/registry.

## Verificação

- `cargo test -p edger-orchestrator --test security_operational` — passou; 8 testes.
- `cargo test -p edger-orchestrator --test admin_workers_plugins` — passou; 6 testes.
- `cargo test -p edger-isolation --features wasm wasm::wasi::tests::filters_sensitive_worker_env -- --exact` — passou; 1 teste.
- `cargo test --workspace` — passou.
- `cargo clippy --workspace -- -D warnings` — passou.
- `cargo fmt -- --check` — passou.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` — passou; 8 epics / 39 stories; 0 referências quebradas; `bun test` pulado porque não há suíte JS/TS raiz.
- `ROOT_API_KEY=test-root PORT=19086 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger` + curl local — passou:
  - `GET /api/admin/workers` sem key preservou `x-request-id: trace-runtime-08-03` e retornou `401`.
  - `POST /api/admin/workers/hello-world/disable` com root, `Sec-Fetch-Mode` e sem `Origin` retornou `403 CSRF_DENIED`.
  - `POST /api/admin/workers/hello-world/disable` com root e `Origin` same-origin retornou `501 NOT_IMPLEMENTED`.
  - `POST /api/admin/workers/hello-world/disable` com root e `x-edger-internal: true` retornou `501 NOT_IMPLEMENTED`.

## Riscos restantes

- Enable/disable real ainda precisa de persistência atômica e reload controlado.
- Superfícies futuras de files/uploads/plugins devem herdar os mesmos guards de namespace e CSRF quando forem criadas.
- Deno CLI bridge ainda não passa `manifest.env`; quando isso mudar, deve reutilizar `edger_core::is_sensitive_env_key`.

## Próximo

Executar 08.04 `planning/edger/epics/08-valor-buntime/04-servicos-de-estado-turso-kv-queue.md`, usando os guards da 08.03 como fronteira para serviços com estado.
