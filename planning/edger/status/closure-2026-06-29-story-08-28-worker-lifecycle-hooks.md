# Closure: Story 08.28 worker lifecycle hooks

Date: 2026-06-29
Scope: per-story closure for Epic 08 value parity

## Plan Status
- [x] Adicionar contrato core de lifecycle de worker.
- [x] Integrar lifecycle hooks ao orchestrator sem acoplar `edger-worker`.
- [x] Cobrir dispatch real e short-circuit com testes.
- [x] Atualizar matriz, overview, roadmap e checkpoint do Epic 08.
- [x] Registrar evidência executável.
- [x] Rodar planning gate e Rust gate completos.

## Files Changed
- `edger-core/src/extension.rs`
- `edger-orchestrator/src/hooks.rs`
- `edger-orchestrator/src/lib.rs`
- `edger-orchestrator/src/pipeline.rs`
- `edger-orchestrator/tests/registry_hooks.rs`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/epics/08-valor-buntime/28-worker-lifecycle-hooks.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`
- `planning/edger/status/evidence/story-08-28-runtime.txt`

## Scope Drift
- Nenhum drift fora da Story 08.28. As mudanças se limitam a contrato de hooks, chamada no pipeline, testes e artefatos de paridade.
- `edger-worker` permaneceu sem dependência de extensões.
- Loader dinâmico, reload/rescan e persistência completa de manifesto continuam fora desta story.

## Result
- `ExtensionHook` agora cobre `onWorkerDispatch`, `onWorkerComplete` e `onWorkerError`.
- `Middleware` tem métodos default para lifecycle de worker, preservando extensões existentes.
- O pipeline dispara lifecycle somente ao redor de dispatch real pelo `WorkerPool`.
- Short-circuit de `on_request` retorna antes de lifecycle de worker.
- `Hooks request/response/lifecycle` passou para `tested` na matriz.

## Breaking Changes
- Nenhuma quebra esperada: os novos métodos têm implementação default.
- Nenhuma mudança de schema, migration ou deploy externo.

## Tests
- PASS: `cargo test -p edger-orchestrator --test registry_hooks`
- PASS: `cargo test --workspace`

## Type Checking
- PASS: `cargo check --workspace` via `run-gates.sh`

## Linting
- PASS: `cargo clippy --workspace -- -D warnings`
- PASS: `cargo fmt -- --check`

## Database
- Nenhuma migration.
- Nenhuma alteração de storage runtime.

## Dependencies
- Nenhuma dependência nova.

## Performance Impact
- Impacto runtime baixo: três loops sobre middlewares ativos em requests que chegam ao worker dispatch.
- Middlewares existentes usam métodos default no-op.

## Next Steps
- Continuar a próxima linha `must partial` da matriz.
- Candidatos imediatos: APIs de extensões reload/rescan, gateway/proxy gaps ou logging assertions globais.
