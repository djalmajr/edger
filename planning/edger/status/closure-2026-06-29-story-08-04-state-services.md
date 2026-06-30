# Closure — Story 08.04 Serviços de estado

**Data:** 2026-06-29  
**Story:** `planning/edger/epics/08-valor-buntime/04-servicos-de-estado-turso-kv-queue.md`  
**Epic:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Resultado

Story 08.04 concluída como serviços de estado v1. O edger agora possui contratos puros para bindings de SQL durável, KV e queue, providers locais testados e injeção explícita de descriptors para workers autorizados. A entrega não copia Buntime: preserva o valor observável de storage/queue com fronteira Rust-native. Remoto/sync foi reclassificado para o Epic 09 como provider externo substituível; retries avançados seguem como lacuna de queue.

## Entregue

- `edger-core/src/bindings.rs` com `BindingManifest`, `BindingDescriptor`, `BindingSet`, `StateValue`, `SqlRow`, `DurableSqlProvider`, `KeyValueProvider` e `QueueProvider`.
- Manifest/config aceitam `bindings` e preservam `durableSql`, `keyValue` e `queue` no `WorkerConfig`.
- `edger-ext-turso` implementa `LocalSqliteProvider` sobre SQLite por namespace, em memória ou file-backed; `LocalTursoProvider` permanece como alias legado.
- `edger-ext-keyval` implementa KV `set/get/delete` com versionstamp/TTL e queue `enqueue/dequeue/ack` sobre `DurableSqlProvider`.
- `edger-orchestrator/src/service_bindings.rs` resolve bindings por worker, aplica namespace deny-by-default e serializa `x-edger-bindings`.
- `edger-orchestrator/tests/state_services.rs` prova worker Deno recebendo bindings explícitos, ausência de header quando não há bindings e `403` para worker público com bindings.
- `workers/state-demo` documenta o fixture operacional do contrato.
- `docs/developers/06-operacao-e-testes.adoc` e `planning/edger/docs/value-parity-matrix.md` registram o v1 e suas lacunas.

## Drift de escopo

- `edger-ext-turso` é local/single-node; Turso remoto, sync e credenciais operacionais ficam no Epic 09 como provider externo sobre `DurableSqlProvider`.
- O worker recebe descriptors no header; SDK/helper para executar operações de estado diretamente a partir do worker fica para 08.06.
- Queue v1 cobre enqueue/dequeue/ack, mas não implementa retry/backoff/DLQ completo.
- KV v1 mantém versionstamp incremental e TTL, mas OCC forte ainda não é exposto como operação pública.

## Verificação

- `cargo test -p edger-core` — passou.
- `cargo test -p edger-ext-turso` — passou; 3 testes de provider local.
- `cargo test -p edger-ext-keyval` — passou; 3 testes de KV/queue.
- `cargo test -p edger-orchestrator --test state_services` — passou; 3 testes de binding/deny-by-default.
- `cargo test --workspace` — passou.
- `cargo clippy --workspace -- -D warnings` — passou.
- `cargo fmt -- --check` — passou.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` — passou; 8 epics / 39 stories; 0 referências quebradas; `bun test` pulado porque não há suíte JS/TS raiz.
- `ROOT_API_KEY=test-root PORT=19084 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger` + curl local — passou:
  - `GET /state-demo` com root retornou `bindings` para `durableSql`, `keyValue` e `queue`.
  - `GET /state-demo` sem key retornou `401 UNAUTHORIZED`.
- Browser embutido — tentativa realizada, mas a navegação local para `127.0.0.1:19084` e `localhost:19084` foi bloqueada pelo próprio cliente com `ERR_BLOCKED_BY_CLIENT`; a evidência funcional manual ficou em curl.

## Riscos restantes

- Remote Turso/libsql deve manter o mesmo contrato `DurableSqlProvider` sem vazar transporte para workers, conforme Epic 09.
- Provider lookup/registry de serviços deve ser consolidado em 08.06 antes de expor SDK de worker.
- Retry, backoff e DLQ devem ser tratados como evolução de queue, não como promessa implícita do v1.

## Próximo

Executar 08.05 `planning/edger/epics/08-valor-buntime/05-shell-gateway-e-experiencia-de-apps.md` ou 08.06 `planning/edger/epics/08-valor-buntime/06-modelo-de-extensoes-e-bindings.md`, dependendo se a próxima prioridade for composição de apps ou estabilização de providers/bindings.
