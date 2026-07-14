# Story 05.03: Pipeline de requisições (build_pipeline, hook chain stub, SerializedRequest)

**Origin:** `planning/edger/epics/05-orquestrador/00-overview.md`

## Context
- **Problema:** Servidor e router existem isolados; não há fluxo unificado request → hooks → pool.
- **Objetivo:** Implementar `build_pipeline` e conversão hyper/axum → `SerializedRequest` com cadeia de hooks stub.
- **Valor:** Esqueleto do pipeline Buntime em Rust, testável com pool mock antes de execução real.
- **Restrições:** Hook chain pode ser stub vazio nesta story; registry completo na 05.05; auth na 05.04.

## Traceability
- **Source docs:** `planning/edger/design.md` (Main Binary & Composition, API hooks), `planning/edger/design.md (contratos runtime; ai-memory zommehq/buntime)`
- **Design PR:** PR 6 (pipeline + composition sketch)
- **Depende de:** Stories 05.01, 05.02, Epic 02 (`SerializedRequest`/`SerializedResponse`), Epic 04 (`WorkerPool::fetch`)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `crates/edger-orchestrator/src/pipeline.rs` | criar | `build_pipeline`, `OrchestratorService` |
| `crates/edger-orchestrator/src/wire.rs` | criar | hyper Request → SerializedRequest |
| `crates/edger-orchestrator/src/context.rs` | criar | `RequestContext`, request_id |
| `crates/edger-orchestrator/src/server.rs` | alterar | montar service do pipeline |
| `crates/edger-orchestrator/tests/pipeline_integration.rs` | criar | E2E mock |
| `crates/edger-orchestrator/src/lib.rs` | alterar | exports |

## Detail

### AS-IS
Handlers health isolados; sem `build_pipeline`; sem conversão wire.

### TO-BE
- `build_pipeline(registry, pool, manifests) -> OrchestratorService` (tower `Service` ou axum `Router` nested)
- Fluxo por request:
  1. Converter request HTTP → `SerializedRequest` (method, url, headers, body bytes)
  2. `resolve_route` (05.02)
  3. Se reserved → handler interno (health já tratado; `/api` stub 404 ou proxy futuro)
  4. Hook chain stub (`run_on_request` no-op retorna `None`)
  5. `pool.fetch(worker_dir, config, req, kind_hint)` com mock isolate
  6. Converter `SerializedResponse` → HTTP response
- `RequestContext`: `request_id`, `principal: Option<ApiKeyPrincipal>` (vazio até 05.04)
- Erros mapeados para status HTTP + body JSON tipado

### Escopo
- **In:** wire conversion, pipeline wiring, dispatch mock, testes E2E
- **Out:** auth gate real, registry com prioridade, short-circuit (05.04–05.05)

### Critérios de aceite
- [x] `SerializedRequest` roundtrip preserva method, path, headers críticos, body
- [x] Request a worker mock retorna resposta do pool mock
- [x] Reserved paths não chamam `pool.fetch`
- [x] `build_pipeline` compõe com tower layers (tracing, body limit stub)
- [x] Teste E2E: fixture worker dir + mock response 200

## Pendências
- `PluginBase` retorna 501 até dispatch de plugin (Epic 06/07).
- `HookRunner` stub vazio; registry + short-circuit na 05.05.
- `RequestContext.principal` sempre `None` até 05.04.
- Bin `edger` usa `ManifestIndex` vazio; carga de dirs em 07.01.
- Body limit via `MAX_BODY_BYTES` (4 MiB); tower layer dedicado opcional depois.

### Dependências
- Stories 05.01, 05.02
- Epic 04: `WorkerPool::fetch` funcional com mock

## Test-first plan
1. **Red:** `hyper_to_serialized(GET /foo)` → struct com path `/foo`
2. **Red:** `serialized_to_hyper(200, body)` → Response válida
3. **Red:** pipeline + mock pool → worker fixture retorna `"ok"`
4. **Red:** `/health` via pipeline → não invoca pool
5. **Green:** implementar `wire.rs` + `pipeline.rs`
6. **Refactor:** separar `DispatchDecision` do transporte HTTP

**Nível:** integração (`pipeline_integration.rs`) + unit (`wire.rs`)

## Tasks
- [x] Implementar `wire.rs` (hyper/axum ↔ Serialized*)
- [x] Criar `RequestContext` e propagar `X-Request-Id`
- [x] Implementar `OrchestratorState` + `build_pipeline` (axum fallback handler)
- [x] Integrar `resolve_route` antes do dispatch
- [x] Chamar `WorkerPool::fetch` para rotas Worker
- [x] Stub `HookRunner` vazio (interface para 05.05)
- [x] Mapear erros `CoreError`/`WorkerError` → HTTP status
- [x] Teste E2E com worker index + mock pool
- [x] Conectar pipeline ao bin `edger.rs`

## Verification
```bash
cargo test -p edger-orchestrator pipeline
cargo test -p edger-orchestrator
cargo clippy -p edger-orchestrator -- -D warnings
bun test
```