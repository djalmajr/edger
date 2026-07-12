# Story 08.08: Provas de migração e fechamento da matriz de valor

**Status:** completed

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** Sem provas finais, a matriz pode virar documentação aspiracional. O objetivo da Epic 08 é demonstrar que edger entrega no mínimo o mesmo valor relevante que Buntime entrega hoje.
- **Objetivo:** Rodar fluxos representativos migrados ou espelhados do Buntime e fechar a matriz com evidência.
- **Valor:** O projeto passa a ter critério de pronto para valor, não apenas para implementação.
- **Restrições:** Cada prova deve usar edger real; stubs só são aceitos quando a matriz marcar o item como partial com razão explícita.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `planning/edger/runtime-functional-plan.md`
- **Buntime refs:** apps `todos`, `shell`, `platform`, plugins gateway/keyval/turso/cron em `<buntime-repo>/`
- **Prototype refs:** Browser local para TodoMVC/shell quando visual.
- **Business rules:** prova deve refletir fluxo de usuário/operador, não apenas unidade técnica.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/tests/value_parity.rs` | create | Suite E2E de fluxos must-have |
| `edger-orchestrator/src/manifest_index_stub.rs` | edit | Proteger roteamento contra `base: ""` aprendido do Buntime |
| `workers/value-parity/todos/` | create | Fixture visual para Browser em `/todos` |
| `planning/edger/docs/value-parity-matrix.md` | edit | Atualizar status e evidências finais |
| `planning/edger/docs/compat-matrix.md` | edit | Sincronizar lacunas técnicas encontradas |
| `planning/edger/status/evidence/story-08-08-runtime.txt` | create | Evidência runtime manual e Browser |
| `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md` | create | Checkpoint de fechamento quando executado |
| `planning/edger/roadmap.md` | edit | Marcar Fase 8 conforme resultado |
| `README.md` | edit | Atualizar status de valor após fechamento |

## Detail

### AS-IS
- TodoMVC já validou uma fatia importante de SPA via Deno CLI bridge, mas isso não cobre gestão, estado, shell, gateway e operação.
- Compat matrix e value matrix ainda precisam de evidência por fluxo.

### TO-BE
- Suite ou runbook executa provas representativas:
  - `todos` SPA carrega e serve assets sem loader preso.
  - Worker protegido responde com auth e nega sem auth.
  - App com estado usa SQL/KV por binding.
  - App shell-hosted navega sob base path.
  - Gateway/proxy aplica regra sem bypass de auth.
  - Job agendado dispara quando cron foundation estiver disponível.
- Cada prova escreve resultado em status/evidence ou checkpoint.
- Matriz fica fechada: tested, partial ou gap com decisão explícita.

### Scope
- **In:** testes E2E, browser validation onde visual, matriz final e checkpoint.
- **Out:** migração completa de todos os apps Buntime, benchmark de produção, UI final.

### Story-time plan

**Modo:** plan-then-implement autorizado pelo objetivo ativo `prossiga com o epic 8`.

**Decisões travadas:**

| Decisão | Escolha | Motivo |
|---|---|---|
| Provas mínimas | SPA `/todos`, worker protegido, state bindings, shell/gateway, CORS/auth, `base: ""` guard | Cobre valor operacional real sem fingir paridade total |
| Fixture visual | `workers/value-parity/todos` com `visibility: public` | Permite validação Browser direta em `/todos` sem credencial manual |
| Cron | Marcar `partial/planned` | A foundation de cron ainda não está pronta; não inventar sucesso sem execução |
| Gateway/proxy | Provar middleware/CORS e shell routing; proxy externo fica lacuna explícita | A story 08.05 entregou gateway v1, não proxy/cache/rate-limit persistente |
| Buntime gotcha | `base: ""` deve ser ignorado como app surface | Evita repetir o bug de plugin puro sequestrar navegação de workers |

**Test-first plan:**

- Primeiro teste novo: `edger-orchestrator/tests/value_parity.rs` deve falhar enquanto a suite não existir e provar contratos de valor por fluxo.
- Casos:
  - SPA TodoMVC-equivalente serve documento, asset e fallback sob `/todos`;
  - worker protegido retorna `401` sem auth e `200` com root key;
  - app stateful recebe descritores SQL/KV/queue por binding;
  - shell serve navegação de documento, iframe bypassa para app, path admin não é interceptado;
  - preflight CORS do gateway não bypassa auth e funciona com auth;
  - manifesto com `base: ""` não vira shell nem sequestra navegação.
- Testes de baixo valor evitados: snapshots longos de HTML, duplicar cada teste unitário existente, ou marcar cron como verde sem scheduler executável.

### Acceptance criteria
- [x] Pelo menos quatro fluxos must-have rodam em edger real com evidência.
- [x] TodoMVC ou app equivalente é validado via Browser quando servidor local estiver disponível.
- [x] Lacunas restantes são marcadas como partial/gap, com owner story ou decisão later.
- [x] Roadmap e README refletem status final da Fase 8.
- [x] Gate Rust e gate de planejamento passam depois das evidências.

### Dependencies
- Stories 08.02-08.07.
- Epic 07 para execução JS/Wasm/shell/cron conforme fluxo escolhido.

## Tasks
- [x] Criar fixture visual `workers/value-parity/todos/`.
  - Done when: `/todos` servir HTML, asset e fallback em runtime local.
- [x] Implementar testes E2E por fluxo must-have.
  - Done when: `cargo test -p edger-orchestrator --test value_parity` cobrir SPA, auth, state, shell/gateway, CORS/auth e `base: ""`.
- [x] Rodar Browser para fluxo visual.
  - Done when: Browser abrir `http://127.0.0.1:19084/todos` e confirmar DOM do app.
- [x] Atualizar matrizes com evidência.
  - Done when: `value-parity-matrix.md` e `compat-matrix.md` não contradisserem as provas executadas.
- [x] Criar checkpoint de fechamento.
  - Done when: checkpoint e evidence registrarem comandos, resultados e lacunas.
- [x] Atualizar roadmap e README.
  - Done when: ambos refletirem Fase 8 em fechamento por valor, com lacunas explícitas.

## Verification
```bash
cargo test -p edger-orchestrator --test value_parity
ROOT_API_KEY=test-root PORT=19084 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
curl -H 'authorization: Bearer test-root' http://127.0.0.1:19084/todos
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
