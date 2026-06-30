# Closure: Story 08.27 deploy layout check

Date: 2026-06-29
Scope: per-story closure for Epic 08 value parity

## Plan Status
- [x] Criar checker portátil para o layout local de operação/deploy.
- [x] Integrar o checker ao gate de planejamento.
- [x] Atualizar matriz, overview, roadmap e checkpoint do Epic 08.
- [x] Registrar evidência executável.
- [x] Rodar planning gate e Rust gate completos.

## Files Changed
- `planning/edger/scripts/deploy-layout-check.py`
- `planning/edger/scripts/run-gates.sh`
- `docs/developers/06-operacao-e-testes.adoc`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/epics/08-valor-buntime/27-deploy-layout-check.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`
- `planning/edger/status/evidence/deploy-layout-check.txt`
- `planning/edger/status/evidence/story-08-27-runtime.txt`

## Scope Drift
- Nenhum drift fora da Story 08.27. As mudanças mapeiam para checker, integração no gate, documentação operacional local e artefatos de paridade.
- Deploy remoto, PVC e Kubernetes permaneceram fora do escopo desta story.
- Turso remoto/sync continuou tratado como provider externo substituível no Epic 09, não como implementação interna obrigatória do edger.

## Result
- `Filesystem/deploy layout` passou para `tested` na matriz para o escopo local/single-node.
- `run-gates.sh` agora executa `planning/edger/scripts/deploy-layout-check.py --repo .` e grava `planning/edger/status/evidence/deploy-layout-check.txt`.
- O checker valida tokens estáveis do runbook: roots de workers, arquivos de estado/auth, status de extensões, provider boundaries, probes, backup/restore, Admin API, gateway e gates obrigatórios.

## Breaking Changes
- Nenhuma quebra de contrato runtime.
- Nenhuma mudança de schema, migration ou deploy externo.

## Tests
- PASS: `python3 planning/edger/scripts/deploy-layout-check.py --repo .`
- PASS: `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`
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
- O checker usa apenas Python stdlib.

## Performance Impact
- Impacto runtime: nenhum.
- Impacto nos gates: pequeno acréscimo de leitura textual do runbook operacional.

## Next Steps
- Continuar a próxima linha `must partial` da matriz.
- Candidatos imediatos: lifecycle hooks completos, APIs de extensões reload/rescan ou gateway/proxy gaps.
