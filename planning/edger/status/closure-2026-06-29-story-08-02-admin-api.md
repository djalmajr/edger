# Closure — Story 08.02 API operacional

**Data:** 2026-06-29  
**Story:** `planning/edger/epics/08-valor-buntime/02-api-operacional-workers-e-plugins.md`  
**Epic:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Resultado

Story 08.02 concluída como API operacional v1. O edger agora expõe inventário root-only de workers, extensões e API keys, além de sessão para qualquer key válida. Mutações de enable/disable nasceram roteadas e protegidas, retornando `501` tipado nesta story; 08.11 e 08.13 substituíram esses stubs por toggles runtime reais em memória.

## Entregue

- `edger-core/src/admin.rs` com DTOs puros para workers, extensões, keys, sessão, erro e mutação.
- `edger-orchestrator/src/admin_api.rs` com rotas `/api/admin/session`, `/api/admin/workers`, `/api/admin/extensions`, `/api/admin/keys` e mutações protegidas de worker.
- `build_pipeline` monta o admin router antes do fallback, impedindo `/api/admin/*` de cair em worker dispatch ou `API_STUB`.
- `ManifestIndex`, `ExtensionRegistry` e `SqliteApiKeyStore` expõem inventário ordenado e seguro sem vazar raw key ou hash.
- `docs/developers/06-operacao-e-testes.adoc` documenta o contrato operacional e exemplos curl.
- `planning/edger/docs/value-parity-matrix.md` marca as linhas de API operacional como `partial` com evidência automatizada.

## Drift de escopo

- Sem mutação real de enable/disable nesta story. A rota existe, exige root e retorna `501` para evitar edição frágil de manifest ou registry antes de uma persistência segura.
- Superseded: worker enable/disable runtime foi entregue em 08.11 e extension enable/disable runtime foi entregue em 08.13; persistência continua fora do escopo.
- A API de keys lista metadata, mas não cria nem revoga chaves ainda; isso segue para as próximas fatias de segurança/estado.
- A story não introduziu UI administrativa nem OpenAPI formal.

## Verificação

- `cargo test -p edger-orchestrator --test admin_workers_plugins` — passou; 6 testes.
- `cargo test --workspace` — passou.
- `cargo clippy --workspace -- -D warnings` — passou.
- `cargo fmt -- --check` — passou.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` — passou; 8 epics / 39 stories; 0 referências quebradas; `bun test` pulado porque não há suíte JS/TS raiz.
- `ROOT_API_KEY=test-root PORT=19085 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger` + curl local — passou:
  - `GET /api/admin/session` com root retornou `200` e principal root.
  - `GET /api/admin/workers` com root retornou `200` e inventário dos workers em `workers/`.
  - `GET /api/admin/extensions` com root retornou `200` com `auth` e `gateway`.
  - `GET /api/admin/keys` com root retornou `200` e `keys: []` no store em memória.
  - `GET /api/admin/workers` sem key retornou `401`.
  - `POST /api/admin/workers/todos/disable` com root retornou `501 NOT_IMPLEMENTED`.
- `git diff --check` — passou.

## Riscos restantes

- 08.03 deve fechar permissões namespace-aware, CSRF/internal calls, request IDs e limites antes de mutações stateful browser-facing.
- Enable/disable real precisa de persistência atômica de manifests/registry e estratégia de reload controlado.
- API de keys ainda não cobre rotação/revogação; o contrato atual é inventário operacional seguro.

## Próximo

Executar 08.03 `planning/edger/epics/08-valor-buntime/03-seguranca-e-identidade-operacional.md`, usando a Admin API v1 como superfície a proteger e expandir.
