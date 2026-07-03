# Story 17.C: Remover estado + service bindings

**Origin:** `planning/edger/epics/17-edger-minimalista/00-overview.md`

## Context

- **Problema:** o edger injeta KV/queue/SQL nos workers via service bindings sobre `DurableSqlProvider` (`edger-ext-keyval`, `edger-ext-turso`, `edger-ext-turso-remote`). Estado da aplicação não é função do runtime.
- **Objetivo:** deletar os providers de estado e o mecanismo de bindings; o worker conecta **direto** no backend que escolher (libSQL/Turso, Deno KV Connect, Postgres+PgBouncer) usando env/secrets injetados + egress.
- **Valor:** worker escolhe seu banco; edger para de carregar/operar estado.
- **Restrições:** manter injeção de env/secrets (filtrada) e `--allow-net` (egress) — é assim que o worker recebe credencial e alcança o backend.

## Traceability
- `edger-ext-keyval`, `edger-ext-turso`, `edger-ext-turso-remote` (deletar); `edger-core/src/bindings.rs`; `edger-orchestrator/src/service_bindings.rs`, `registry.rs` (registro de providers)

## Files
| Path | Action | Reason |
|---|---|---|
| `edger-ext-keyval/`, `edger-ext-turso/`, `edger-ext-turso-remote/` | delete | Estado sai do edger |
| `edger-orchestrator/src/service_bindings.rs` | delete | Sem bindings |
| `edger-core/src/bindings.rs` | delete | Vocabulário de bindings some |
| `edger-orchestrator/src/bin/edger.rs` | edit | Remover wiring de sql/kv/queue provider; manter env/secrets + egress |
| `edger-core/src/config.rs` | edit | Remover `bindings` do `WorkerConfig` |

## Detail
### Scope
- **In:** deletar 3 crates de estado + bindings; limpar wiring no boot; remover `bindings` do manifest/config.
- **Out:** o serviço de estado externo em si (worker escolhe; Deno KV multi-tenant é projeto à parte).

### Acceptance criteria
- [ ] Crates de estado e `service_bindings`/`bindings` deletados; workspace compila sem eles.
- [ ] Worker ainda recebe env/secrets (filtrados) e tem egress — prova: worker conecta num libSQL/Postgres externo com credencial via env.
- [ ] Manifesto sem `bindings` carrega normal.

### Dependencies
- Story 17.B

## Tasks
- [ ] Deletar crates + `service_bindings.rs` + `bindings.rs`; limpar boot e `WorkerConfig`.
- [ ] E2E/live: worker soberano conectando num backend externo por env.

## Verification
```bash
cargo build --workspace
# live: worker que abre conexão a um Postgres/libSQL externo com URL/token via env do manifest
```
