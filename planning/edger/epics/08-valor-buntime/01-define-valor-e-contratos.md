# Story 08.01: Definir valor, contratos e matriz Buntime -> edger

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Status
completed (2026-06-29) — matriz inicial expandida a partir das fontes Buntime locais e vinculada às próximas stories da Epic 08.

## Context
- **Problema:** A compatibilidade atual mede principalmente execução e manifests. Buntime, porém, entrega valor operacional mais amplo: gestão, segurança, serviços, shell, plugins e evidência.
- **Objetivo:** Criar uma matriz de valor que traduza capacidades Buntime em contratos observáveis edger, com prioridade e evidência esperada.
- **Valor:** Evita cópia as-is e dá critério objetivo para decidir quais próximas histórias realmente aproximam o edger do valor entregue pelo Buntime.
- **Restrições:** Não usar Bun como fallback; não aceitar “paridade” sem fluxo observável; preservar Epic 07 como dependência técnica.

## Traceability
- **Source docs:** `planning/edger/docs/compat-matrix.md`, `planning/edger/runtime-functional-plan.md`, `planning/edger/docs/pendencies-epic-07.md`
- **Buntime refs:** docs locais de runtime, worker-pool, plugins, storage e security em `<buntime-repo>/apps/site/src/content/docs/`
- **Prototype refs:** none.
- **Business rules:** valor é descrito por fluxo e contrato, não por estrutura interna do Buntime.

## Files

| Path | Action | Reason |
|---|---|---|
| `planning/edger/docs/value-parity-matrix.md` | edit | Matriz canônica de valor Buntime -> edger |
| `planning/edger/docs/compat-matrix.md` | edit | Cross-link entre compatibilidade técnica e valor de produto |
| `planning/edger/roadmap.md` | edit | Registrar Fase 8 como sequência planejada |
| `README.md` | edit | Atualizar contagem e próximo foco de planejamento |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | edit | Ajustar prioridades conforme classificação final |

## Detail

### AS-IS
- Há matriz de compatibilidade técnica em `planning/edger/docs/compat-matrix.md`.
- Não há critério único para dizer que edger entrega o mesmo valor operacional do Buntime.
- Prioridades ainda podem cair na tentação de copiar plugins ou rotas sem provar fluxo real.

### TO-BE
- `value-parity-matrix.md` contém colunas: capacidade Buntime, valor entregue, contrato edger, prioridade, status, evidência, decisão de escopo.
- Cada linha must-have aponta para uma story da Epic 08 ou para uma story da Epic 07 quando a fundação ainda falta.
- O fechamento de histórias futuras atualiza a matriz com evidência.
- Itens later ficam explícitos para não mascarar lacunas.

### Approach

| Decisão story-time | Escolha | Motivo |
|---|---|---|
| Unidade de paridade | Valor observável | Evita copiar implementação Buntime e força evidência por fluxo |
| Granularidade da matriz | Capacidade com owner story | Permite que 08.02-08.08 executem sem rediscutir escopo |
| Status inicial | Conservador | `tested` só quando há evidência atual no repo; caso contrário `partial`, `planned`, `gap` ou `deferred` |
| Escopo de UI | Contratos antes de UI final | Buntime tem CPanel/shell; edger deve primeiro estabilizar API e protocolo |

### Scope
- **In:** levantamento de capacidades, classificação must/should/later, matriz de evidência, cross-links com compat matrix.
- **Out:** implementar API, serviços ou UI nesta story.

### Acceptance criteria
- [x] Matriz lista runtime/pool, worker lifecycle, API operacional, segurança, storage, plugin model, gateway/shell, observabilidade, deploy e provas de migração.
- [x] Cada linha possui prioridade e evidência esperada.
- [x] Must-have sem fundação técnica aponta para dependência Epic 07.
- [x] `compat-matrix.md` referencia a matriz de valor.
- [x] Roadmap e README não ficam com contagem de epics/stories antiga.

### Dependencies
- Epic 07 precisa permanecer como fonte de pré-requisitos técnicos.

## Test-first plan
- **Behavior:** O artefato deve impedir que uma story futura declare “valor Buntime” sem owner, prioridade e evidência.
- **First failing check:** `planning/edger/scripts/run-gates.sh` falharia se a nova matriz quebrasse referências; `rg` falharia se README/roadmap/compat não apontassem para Fase 8 e `value-parity`.
- **Preferred level:** Planning lint + path-preflight + inspeção por `rg`; não há código de runtime nesta story.
- **Avoid:** Testes Rust novos para uma mudança documental; eles não provariam a regra de produto.

## Tasks
- [x] Revisar docs Buntime locais e extrair capacidades por fluxo de operador/usuário.
  - Done when: runtime, worker-pool, plugin-system, storage, security e performance forem considerados.
- [x] Editar `planning/edger/docs/value-parity-matrix.md` com classificação inicial.
  - Done when: cada linha tiver capacidade, valor, contrato edger, prioridade, status, owner e evidência esperada.
- [x] Cross-linkar `planning/edger/docs/compat-matrix.md`.
  - Done when: a matriz técnica distinguir compatibilidade técnica de paridade de valor.
- [x] Ajustar `planning/edger/roadmap.md` e `README.md`.
  - Done when: ambos refletirem 8 epics / 39 stories e a Fase 8.
- [x] Validar que todas as linhas must-have apontam para story ou evidência.
  - Done when: nenhuma linha `must` ficar sem owner.

## Verification
```bash
rg -n "value-parity|Paridade de Valor|Fase 8" README.md planning/edger
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Verification results
- `rg -n "value-parity|Paridade de Valor|Fase 8" README.md planning/edger` — passou; README, roadmap, compat matrix e Epic 08 referenciam a matriz.
- `python3 planning/edger/scripts/refinement-lint.py --scope planning/edger/epics/08-valor-buntime --repo .` — passou; 0 red flags.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` — passou; 8 epics / 39 stories; 0 referências quebradas.
- `cargo test --workspace` — passou.
- `cargo clippy --workspace -- -D warnings` — passou.
- `cargo fmt -- --check` — passou.
