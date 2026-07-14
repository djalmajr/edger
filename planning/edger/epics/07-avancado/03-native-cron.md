# Story 07.03: Cron nativo (tokio-cron scheduler)

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`

## Context
- **Problema:** Manifests Buntime suportam `cron[]` com schedules que disparam requisições internas; edger ainda não tem scheduler Rust nativo — apenas stub ou ausência.
- **Objetivo:** Implementar `CronScheduler` com tokio-cron (ou crate equivalente) no orchestrator que dispara HTTP interno para workers conforme manifest, com auth interna e lifecycle no shutdown.
- **Valor:** Jobs agendados rodam sem depender de worker long-lived ou cron externo; alinha decisão do usuário (scheduler nativo Rust, core-driven).
- **Restrições:** Credencial interna para bypass de hooks públicos onde aplicável; cron jobs respeitam `enabled` e namespace; graceful shutdown cancela tasks.

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 11, CronJob, Resolved Decisions — native Rust scheduler)
- **Design PR:** PR 11
- **Buntime refs:** `planning/edger/design.md (contratos runtime; ai-memory zommehq/buntime)` (cron internal requests), `WorkerManifest.cron`
- **Depende de:** `01-full-manifests-kinds.md`, Epic 05 (HTTP client interno / pipeline)

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/src/cron.rs` | create | `CronScheduler`, registro de jobs, tick → internal HTTP |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Métrica Prometheus inclui contador do scheduler |
| `crates/edger-orchestrator/src/manifest_index_stub.rs` | edit | Extrair `cron[]` de cada `WorkerRef` habilitado |
| `crates/edger-core/src/manifest.rs` | no-op | Tipo `CronJob` já existia com `schedule`, `method`, `path` |
| `crates/edger-orchestrator/src/bin/edger.rs` ou `main.rs` | edit | Start/stop scheduler no lifecycle do servidor |
| `crates/edger-orchestrator/Cargo.toml` | no-op | Scheduler usa `tokio::time` já disponível |
| `crates/edger-orchestrator/tests/cron_scheduler_test.rs` | create | Job dispara request; disabled worker skip |
| `workers/examples/cron-worker/` | create | Manifest com cron + handler que incrementa contador |

## Detail

### AS-IS
- Campo `cron` existia em tipos core sem scheduler ativo.
- Nenhum disparo periódico no processo edger.
- Buntime usava internal HTTP com credencial — não portado.

### TO-BE
- No startup, após `load_manifests_from_dirs`, `collect_cron_registrations`
  valida e registra cada `CronJob` de workers habilitados.
- Tick executa request HTTP in-process chamando o `Router` Axum clonado, sem
  loop externo.
- Request entra no pipeline com `x-edger-internal: true`, `Authorization:
  Bearer $ROOT_API_KEY` e `x-request-id: cron-...`; a credencial root é
  removida antes da serialização para user code.
- Worker `enabled: false` não registra jobs. Re-enable via overlay runtime exige
  reload/restart para entrar no scheduler porque os jobs são snapshot de
  startup.
- `ctrl_c` chama `scheduler.shutdown().await` antes de `run_on_shutdown` e
  `shutdown_pool`.
- Métricas Prometheus expõem `edger_cron_executions_total` e
  `edger_cron_failures_total`.

### Scope
- **In:** Parser schedule v1, scheduler tokio, internal dispatch, testes,
  shutdown graceful.
- **Out:** Cron distribuído multi-proc (leader election); UI de gestão de jobs; persistência de last-run em DB (futuro ext).
  Full cron grammar permanece para hardening futuro; v1 suporta `@every <duration>`
  para testes/dev e cron de minuto simples (`* * * * *`, `*/N * * * *`, ou
  minuto fixo).

### Acceptance criteria
- [x] Manifest com `cron[]` dispara handler dentro de tolerância de 2s em teste
  usando intervalo curto `@every 25ms`; `*/1 * * * *` é aceito pelo parser como
  intervalo de 1 minuto.
- [x] Request interno carrega header de credencial interna; o bearer root não é
  encaminhado ao worker e `x-edger-internal` continua sem autenticar sozinho em
  rotas públicas/admin.
- [x] Worker disabled não registra jobs; re-enable requer reload documentado.
- [x] Schedule inválido falha no startup com erro claro (não panic silencioso).
- [x] Shutdown cancela jobs pendentes sem leak de tasks tokio.
- [x] Métrica/contador de execuções cron exposto (prep para 07.06).

### Dependencies
- Story 07.01 (manifest loader com cron fields)
- Epic 05 (pipeline HTTP para internal dispatch)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`crates/edger-orchestrator/tests/`, `crates/edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Tipos e parsing
- [x] Reusar `CronJob` em `edger-core` com serde existente.
- [x] Testes unitários do parser schedule v1 (`@every`, `*/N`, rejeição clara).

### Fase 2 — Scheduler
- [x] Implementar `CronScheduler::start` sobre `tokio::time`.
- [x] Internal HTTP chamando pipeline local via `Router::oneshot`.
- [x] Credencial interna via `ROOT_API_KEY` + `x-edger-internal: true`.

### Fase 3 — Lifecycle
- [x] Wire no binary: start após manifests loaded; stop no graceful shutdown.
- [x] Shutdown roda scheduler antes de `run_on_shutdown` e pool shutdown.

### Fase 4 — Testes
- [x] Fixture `workers/examples/cron-worker/` + teste com intervalo curto.
- [x] Teste: job não roda quando worker disabled.

## Verification
```bash
cargo test -p edger-orchestrator -- cron_scheduler
cargo test -p edger-core -- cron_job
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```
