# Story 08.27: Layout local de deploy verificável

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- Problema atual: a matriz ainda marca `Filesystem/deploy layout` como `partial`; o runbook local cobre variáveis, probes, backup e troubleshooting, mas não há gate que impeça regressão documental.
- Objetivo de entrega: transformar o layout local de operação/deploy em contrato verificável por script, mantendo PVC/K8s como later tier fora desta fatia.
- Restrições: não criar deploy real, não alterar infraestrutura externa, não mover Turso remoto/sync para dentro do core e não tratar documentação sem gate como evidência suficiente.
- Referências: `planning/edger/docs/value-parity-matrix.md`, `docs/developers/06-operacao-e-testes.adoc`, `planning/edger/scripts/run-gates.sh`.

## Traceability
- Protótipos/telas: não aplicável.
- Regras de negócio: operador precisa saber onde ficam workers, estado local, API keys, status de extensões, backups e checks de saúde.
- Source docs: `docs/developers/06-operacao-e-testes.adoc`, `planning/edger/epics/08-valor-buntime/07-observabilidade-operacao-e-deploy.md`, `planning/edger/epics/08-valor-buntime/26-extension-status-persistence.md`.

## Files

| Arquivo | Ação | Motivo | Confiança |
|---|---|---|---|
| `planning/edger/scripts/deploy-layout-check.py` | Criar | Validar mecanicamente o contrato operacional local documentado | core |
| `planning/edger/scripts/run-gates.sh` | Alterar | Rodar o checker como parte do gate de planejamento | core |
| `docs/developers/06-operacao-e-testes.adoc` | Alterar | Documentar o checker no fluxo de gates locais | core |
| `planning/edger/docs/value-parity-matrix.md` | Alterar | Marcar layout local como testado e manter PVC/K8s fora do escopo | core |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | Alterar | Registrar Story 08.27 no backlog/status | core |
| `planning/edger/roadmap.md` | Alterar | Atualizar contagem da Fase 8 | core |
| `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md` | Alterar | Atualizar checkpoint de valor | core |
| `planning/edger/status/evidence/story-08-27-runtime.txt` | Criar | Registrar evidência da entrega | core |

## Detail

### Estado atual (AS-IS)
- O runbook operacional documenta launch local, env vars, probes, backup, Admin API, state providers e troubleshooting.
- `run-gates.sh` valida lint/refinamento, path preflight, seções de stories, `bun test` opcional e `cargo check`.
- Nenhum gate confirma que o runbook preserva os termos operacionais mínimos.

### Estado alvo (TO-BE)
- Um checker portátil em Python valida tokens obrigatórios do runbook local.
- `run-gates.sh` executa o checker e grava evidência em `planning/edger/status/evidence/deploy-layout-check.txt`.
- A matriz passa a tratar `Filesystem/deploy layout` como `tested` para o escopo local/single-node, com PVC/K8s explicitamente fora do v1.

### Escopo
- Inclui contrato local: `RUNTIME_WORKER_DIRS`, `EDGER_AUTH_DB`, `EDGER_STATE_DIR`, `EDGER_EXTENSION_STATUS_FILE`, probes, backup local, Admin API e troubleshooting.
- Inclui integração no gate de planejamento.
- Não inclui Kubernetes, PVC, Helm, deploy remoto, CI/CD ou storage distribuído.

### Approach
- Implementar `deploy-layout-check.py` com checks explícitos e saída determinística.
- Integrar o script ao `run-gates.sh` depois do path preflight.
- Atualizar documentação e artefatos de planejamento para apontar a nova evidência.

### Risks and dependencies
- Risco: checker virar busca textual frágil. Mitigação: validar termos operacionais estáveis, não frases longas.
- Risco: overclaiming de deploy. Mitigação: marcar apenas layout local/single-node como tested; PVC/K8s continua later tier.

## Acceptance criteria
- [x] `planning/edger/scripts/deploy-layout-check.py` falha quando o runbook perde variável, endpoint ou backup obrigatório.
- [x] `run-gates.sh` executa o checker e grava evidência em `deploy-layout-check.txt`.
- [x] `Filesystem/deploy layout` fica `tested` na matriz apenas para o escopo local/single-node.
- [x] PVC/K8s e deploy remoto continuam fora do escopo da Story 08.27.

## Test-first plan
- Comportamento a provar: o gate falha se o contrato operacional local estiver ausente e passa com o runbook atual.
- Primeiro teste falhando: executar o checker antes de integrá-lo ao gate e confirmar a cobertura dos termos esperados.
- Nível preferido: script/gate de planejamento.
- Valor do teste: contrato operacional e documentação verificável.
- Testes de baixo valor a evitar: snapshots de documentação inteira ou validação de frases completas.

## Tasks
- [x] Criar checker de layout operacional. **Done when:** script lista checks e falha com status diferente de zero quando falta termo obrigatório.
- [x] Integrar checker ao gate. **Done when:** `run-gates.sh` grava `deploy-layout-check.txt` e falha se o checker falhar.
- [x] Atualizar artefatos de paridade. **Done when:** matriz, overview, roadmap e checkpoint apontam Story 08.27.
- [x] Registrar evidência e closure. **Done when:** evidence/closure citam comandos reais.
- [x] Rodar verificação. **Done when:** Rust gate e planning gate passam.

## Verification
- [x] `python3 planning/edger/scripts/deploy-layout-check.py --repo .`
- [x] `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`
- [x] `cargo test --workspace`
- [x] `cargo clippy --workspace -- -D warnings`
- [x] `cargo fmt -- --check`

## Recommended next step
- Continuar com a próxima linha `must partial` da matriz.
