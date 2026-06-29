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
| `edger-orchestrator/src/cron.rs` | create | `CronScheduler`, registro de jobs, tick → internal HTTP |
| `edger-orchestrator/src/pipeline.rs` | edit | Hook para rotas internas cron (`X-Edger-Internal` / compat Buntime) |
| `edger-orchestrator/src/manifest_loader.rs` | edit | Extrair `cron[]` de cada `WorkerRef` habilitado |
| `edger-core/src/manifest.rs` | edit | Tipo `CronJob` completo (schedule, method, path, headers) |
| `edger-orchestrator/src/bin/edger.rs` ou `main.rs` | edit | Start/stop scheduler no lifecycle do servidor |
| `edger-orchestrator/Cargo.toml` | edit | Dep `tokio-cron-scheduler` ou `cron` + tokio timer |
| `edger-orchestrator/tests/cron_scheduler_test.rs` | create | Job dispara request; disabled worker skip |
| `workers/cron-worker/` | create | Manifest com cron + handler que incrementa contador |

## Detail

### AS-IS
- Campo `cron` pode existir em tipos core sem scheduler ativo.
- Nenhum disparo periódico no processo edger.
- Buntime usa internal HTTP com credencial — não portado.

### TO-BE
- No startup, após `load_manifests_from_dirs`, registrar cada `CronJob` válido no scheduler.
- Tick executa request HTTP in-process (hyper client ou chamada direta ao `Service`) para path/method do job.
- Header interno autenticado (equivalente `X-Buntime-Internal`) bypassa auth pública mas respeita namespace do worker.
- Worker `enabled: false` remove jobs do scheduler (sem restart — watcher ou reload manual documentado).
- `ctrl_c`/shutdown: `scheduler.shutdown().await` antes de pool shutdown.
- Logs estruturados: `cron_job_id`, `worker_name`, `schedule`, `request_id` correlacionado.

### Scope
- **In:** Parser cron schedule, scheduler tokio, internal dispatch, testes, shutdown graceful.
- **Out:** Cron distribuído multi-proc (leader election); UI de gestão de jobs; persistência de last-run em DB (futuro ext).

### Acceptance criteria
- [ ] Manifest com `cron: [{ schedule: "*/1 * * * *", path: "/tick", method: "GET" }]` dispara handler dentro de tolerância de 2s em teste (clock mock ou interval curto).
- [ ] Request interno carrega header de credencial interna; não exposto em rotas públicas.
- [ ] Worker disabled não registra jobs; re-enable requer reload documentado.
- [ ] Schedule inválido falha no startup com erro claro (não panic silencioso).
- [ ] Shutdown cancela jobs pendentes sem leak de tasks tokio.
- [ ] Métrica/contador de execuções cron exposto (prep para 07.06).

### Dependencies
- Story 07.01 (manifest loader com cron fields)
- Epic 05 (pipeline HTTP para internal dispatch)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`edger-orchestrator/tests/`, `edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Tipos e parsing
- [ ] Finalizar `CronJob` em `edger-core` com serde + validação de schedule (cron expression).
- [ ] Testes unitários: parse manifest com múltiplos jobs, timezone UTC default documentado.

### Fase 2 — Scheduler
- [ ] Implementar `CronScheduler::register(worker_ref, jobs)` usando tokio-cron.
- [ ] Internal HTTP client chamando pipeline local (evitar loop externo).
- [ ] Credencial interna via env `RUNTIME_INTERNAL_SECRET` ou synthetic principal root-only.

### Fase 3 — Lifecycle
- [ ] Wire no binary: start após manifests loaded; stop no graceful shutdown.
- [ ] Integrar com `ExtensionRegistry::on_shutdown` se extensões precisam flush.

### Fase 4 — Testes
- [ ] Fixture `workers/cron-worker/` + test com clock acelerado ou `tokio::time::pause`.
- [ ] Teste: job não roda quando worker disabled.

## Verification
```bash
cargo test -p edger-orchestrator -- cron_scheduler
cargo test -p edger-core -- cron_job
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
bun test
```
