# Closure — Story 08.01 Valor e contratos

**Data:** 2026-06-29  
**Story:** `planning/edger/epics/08-valor-buntime/01-define-valor-e-contratos.md`  
**Epic:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Resultado

Story 08.01 concluída. A Epic 08 agora tem uma matriz de paridade de valor que transforma aprendizados do Buntime em capacidades observáveis do edger, sem tratar o Buntime como blueprint de implementação.

## Entregue

- `planning/edger/docs/value-parity-matrix.md` expandida com runtime, manifests, routing, pool, APIs operacionais, segurança, storage, plugins/extensões, shell/gateway, cron, observabilidade, deploy e provas de migração.
- Cada linha da matriz passou a ter valor entregue, contrato edger esperado, prioridade, status atual, owner e evidência esperada ou atual.
- `planning/edger/epics/08-valor-buntime/01-define-valor-e-contratos.md` recebeu status, approach story-time, plano de verificação e checklist fechado.
- `planning/edger/epics/08-valor-buntime/00-overview.md` marca 08.01 como completed e a Epic 08 como in-progress.
- `planning/edger/docs/compat-matrix.md` diferencia compatibilidade técnica de paridade de valor.

## Drift de escopo

- Sem drift funcional: a story era documental/planejamento e não alterou runtime.
- Arquivos em `planning/edger/status/evidence/` foram atualizados pelo gate de planejamento.
- O worktree já continha mudanças anteriores de runtime e documentação; esta closure não as reverte nem as reinterpreta.

## Verificação

- `rg -n "value-parity|Paridade de Valor|Fase 8" README.md planning/edger` — passou.
- `python3 planning/edger/scripts/refinement-lint.py --scope planning/edger/epics/08-valor-buntime --repo .` — passou; 0 red flags.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` — passou; 8 epics / 39 stories; 0 referências quebradas.
- `cargo test --workspace` — passou.
- `cargo clippy --workspace -- -D warnings` — passou.
- `cargo fmt -- --check` — passou.

## Riscos restantes

- A matriz ainda contém muitos itens `planned` e `partial`; ela define o contrato de valor, mas não entrega API, storage, shell/gateway ou observabilidade por si só.
- Fase 8 depende da base técnica da Epic 07 para itens como `routes` export, shell, cron, OTEL e hardening completo.
- A linha `Vhosts/host routing` permanece `gap` até a Story 08.05 decidir o contrato edger.

## Próximo

Executar `/agile-story` para `planning/edger/epics/08-valor-buntime/02-api-operacional-workers-e-plugins.md`, mantendo 08.03 segurança em paralelo conceitual para não desenhar API administrativa sem fronteiras de auth/namespace.
