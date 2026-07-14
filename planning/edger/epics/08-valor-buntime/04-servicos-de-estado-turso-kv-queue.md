# Story 08.04: Serviços de estado Turso, KV e queue

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** Buntime entrega valor por storage durável, KV, filas e providers usados por apps/plugins. edger ainda executa workers, mas não oferece contratos de estado equivalentes.
- **Objetivo:** Criar contratos edger-native para SQL durável, KV e queue, com pelo menos uma implementação local e uma prova por worker.
- **Valor:** Aplicações deixam de ser apenas handlers stateless e passam a migrar fluxos reais que dependem de estado.
- **Restrições:** Não misturar modos de crate; extensões de serviço não dependem de `edger-orchestrator`; bindings são explícitos e deny-by-default.

## Status
completed (2026-06-29) — contratos puros de binding/SQL/KV/queue, provider local `edger-ext-turso` com SQLite local semantics, `edger-ext-keyval` sobre provider SQL, descritores explícitos injetados para workers e fixture `state-demo` concluídos. O v1 entrega estado local/single-node; Turso remoto/sync foi reclassificado para Epic 09 como provider externo substituível. SDK worker operacional e retry/backoff/DLQ completos seguem como lacunas documentadas.

## Traceability
- **Source docs:** `planning/edger/docs/extensions.md`, `planning/edger/docs/value-parity-matrix.md`
- **Buntime refs:** storage docs e manifests de `plugin-turso`, `plugin-keyval`, `plugin-gateway` em `<buntime-repo>/plugins/`
- **Prototype refs:** none.
- **Business rules:** estado precisa de isolamento por namespace e worker.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/bindings.rs` | create | Vocabulário puro para service bindings |
| `edger-ext-turso/Cargo.toml` | create | Extensão SQL durável |
| `edger-ext-turso/src/lib.rs` | create | Provider SQL local via contrato de extensão |
| `edger-ext-turso/tests/local_provider.rs` | create | Provar SQL local durável e namespace independente |
| `edger-ext-keyval/Cargo.toml` | create | Extensão KV/queue |
| `edger-ext-keyval/src/lib.rs` | create | Provider KV e queue mínima |
| `edger-ext-keyval/tests/keyval_queue.rs` | create | Provar KV set/get/delete e queue enqueue/dequeue/ack |
| `crates/edger-orchestrator/src/service_bindings.rs` | create | Injeção de bindings no worker |
| `crates/edger-orchestrator/src/lib.rs` | edit | Exportar módulo de service bindings |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Adicionar header de bindings antes do dispatch |
| `crates/edger-orchestrator/tests/state_services.rs` | create | Worker usando SQL, KV e queue |
| `workers/state-demo/manifest.yaml` | create | Fixture de prova |
| `workers/state-demo/index.ts` | create | Worker de prova por contrato |
| `Cargo.toml` | edit | Adicionar crates de extensão ao workspace |
| `crates/edger-core/src/lib.rs` | edit | Exportar contratos de binding |
| `crates/edger-core/src/config.rs` | edit | Normalizar bindings no `WorkerConfig` |
| `crates/edger-core/src/manifest.rs` | edit | Declarar bindings no manifesto do worker |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar providers e header de binding v1 |
| `planning/edger/docs/value-parity-matrix.md` | edit | Evidência para serviços de estado |

## Detail

### AS-IS
- Auth store existe como preocupação separada; não há contrato geral de binding para apps.
- Extensões atuais demonstram Open/Closed, mas não serviços stateful equivalentes.
- Workers não têm API documentada para acessar SQL/KV/queue providos pelo runtime.

### TO-BE
- `BindingDescriptor` descreve tipo, namespace, permissões e nome público.
- Turso-local oferece operação SQL durável mínima para consumidor autorizado, com separação por namespace.
- KV oferece set/get/delete e isolamento por namespace, usando o provider SQL como backend.
- Queue oferece enqueue/dequeue/ack local, usando o provider SQL como backend.
- Teste de integração prova worker real recebendo binding explícito; testes de provider provam operações SQL/KV/queue.

### Approach

| Decisão story-time | Escolha | Motivo |
|---|---|---|
| SQL provider v1 | `edger-ext-turso` com `LocalSqliteProvider` sobre `rusqlite` (`LocalTursoProvider` como alias legado) | Mantém o contrato SQL durável sem bloquear no provider remoto/sync do Epic 09 |
| Dependency graph | `edger-ext-keyval` depende de contratos em `edger-core`; testes usam `edger-ext-turso` como backend | Evita dependência direta obrigatória entre crates de extensão e mantém provider substituível |
| Binding delivery to workers | Header `x-edger-bindings` com JSON de descritores normalizados | Prova explícita deny-by-default sem embutir SDK worker antes de estabilizar 08.06 |
| Namespace | Binding usa namespace declarado, senão namespace do worker, senão nome do worker | Garante isolamento previsível para workers unscoped e scoped |
| Queue v1 | FIFO local com `enqueue`, `dequeue`, `ack` | Entrega valor mínimo observável; retry/DLQ completo fica para evolução KeyVal |
| SQL surface | `execute`, `query`, `execute_batch` com `StateValue` | Evita abstração multi-adapter e mantém o provider fino como no `plugin-turso` |

### Risks
- `LocalSqliteProvider` não é Turso Sync; ele prova o contrato local/single-pod e mantém `LocalTursoProvider` apenas como alias legado. Sync/remoto fica no Epic 09 como provider externo.
- Header de binding não executa operações por si só; SDK/helper worker real fica para 08.06, mas o worker já recebe capacidade explícita e verificável.
- Queue v1 não cobre retry/backoff/DLQ; isso permanece lacuna documentada na matriz.

### Scope
- **In:** contratos, crates de extensão, fixture worker, testes, documentação de matriz.
- **Out:** cluster multi-pod, replicas Turso Sync completas, garantias exactly-once, painel visual de dados.

### Acceptance criteria
- [x] `edger-core` expõe tipos de binding sem I/O.
- [x] Worker autorizado recebe binding explícito; worker não autorizado recebe erro.
- [x] SQL durável executa fluxo mínimo create/insert/select ou equivalente libsql.
- [x] KV executa set/get/delete com isolamento por namespace.
- [x] Queue possui contrato e teste mínimo de enqueue local.
- [x] Matriz de valor marca storage como tested ou partial com lacuna documentada.

### Dependencies
- Story 08.01 para matriz.
- Story 08.03 para namespace e segurança.
- Story 08.06 para providers mais estáveis, se esta story começar por contrato puro.

## Tasks
- [x] Fase 1 — Contratos puros de binding e estado.
  - Done when: `crates/edger-core/src/bindings.rs`, `manifest.rs`, `config.rs` e `lib.rs` expuserem tipos puros para `durableSql`, `keyValue`, `queue`, `StateValue`, `StateKey`, traits de provider e normalização deny-by-default.
- [x] Fase 2 — Provider SQL local.
  - Done when: `edger-ext-turso` compilar como provider especializado, abrir namespace local/in-memory, executar DDL/insert/select e provar durabilidade local em teste.
- [x] Fase 3 — KV e queue local sobre SQL.
  - Done when: `edger-ext-keyval` implementar set/get/delete isolado por namespace e queue enqueue/dequeue/ack com testes observáveis.
- [x] Fase 4 — Injeção de binding no orchestrator.
  - Done when: pipeline resolver bindings do worker, negar binding sem principal quando necessário e enviar `x-edger-bindings` apenas quando manifesto declarar binding.
- [x] Fase 5 — Fixture worker state-demo.
  - Done when: worker real Deno retornar os bindings recebidos e teste `state_services.rs` provar deny-by-default e binding explícito.
- [x] Fase 6 — Documentação e matriz.
  - Done when: docs operacionais e matriz de valor registrarem `partial/tested` com lacunas de sync/remoto/retry/DLQ.

## Verification
```bash
cargo test -p edger-orchestrator --test state_services
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
