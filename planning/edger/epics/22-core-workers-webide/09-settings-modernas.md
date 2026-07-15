# Story 22.09: Settings modernas, pesquisáveis e coerentes por escopo

**Origin:** `planning/edger/epics/22-core-workers-webide/00-overview.md`

## Context

- **Problema atual:** Settings expõe preferências em uma lista linear, não permite
  busca/reset e mistura valor efetivo com o valor editável do escopo. Algumas
  preferências também prometem comportamento incompleto.
- **Objetivo:** entregar uma superfície moderna com categorias, busca, filtro de
  modificadas, escopos User/Workspace e reset explícito, corrigindo os efeitos já
  suportados pelo workbench.
- **Restrições:** preservar autosave local e deploy explícito; não criar Settings
  fictícias para secrets, runtime ou infraestrutura; manter o editor atual.
- **Referências:** Story 22.08, modelo de Settings existente e levantamento
  validado em 2026-07-15.

## Traceability

- **Tela:** `/webide/?project=<id>`, dialog aberto pela action `Settings`.
- **Regras:** Workspace sobrepõe User; reset de Workspace volta a herdar User;
  reset de User volta ao default; configurações só são exibidas quando possuem
  efeito real.
- **Documentos fonte:** `08-reference-workbench-layout.md` e
  `00-overview.md` deste epic.

## Files

| File | Action | Reason | Confidence |
|---|---|---|---|
| `workers/core/webide/src/lib/settings.ts` | Modify | Modelo, reset, valor por escopo e catálogo | core |
| `workers/core/webide/src/lib/settings.test.ts` | Modify | Provar precedência, reset, filtros e escopos | core |
| `workers/core/webide/src/components/settings-dialog.tsx` | Modify | Busca, lateral, filtro e controles | core |
| `workers/core/webide/src/components/workbench.tsx` | Modify | Consumir reset e corrigir efeitos | core |
| `workers/core/webide/e2e/flows/settings-preferences.md` | Create | Jornada crítica da Settings | core |
| `workers/core/webide/e2e/README.md` | Modify | Registrar o novo fluxo | probable |
| `planning/edger/epics/22-core-workers-webide/00-overview.md` | Modify | Atualizar backlog e estado do epic | core |

## Detail

### Estado atual (AS-IS)

- Quatro seções hardcoded em uma única coluna.
- Tabs User/Workspace mostram valores resolvidos e não expõem reset.
- `Auto Preview` não executa efeito; `Word Wrap` desalinha as duas camadas do
  editor; `Files Exclude` não afeta Search; Theme aceita override Workspace que
  pode contaminar a preferência global.

### Estado alvo (TO-BE)

- Registry tipado dirige categorias, busca, filtros, origem e escopos válidos.
- Menu lateral em desktop e seleção compacta em viewport estreito.
- Cada preferência modificada possui ação de reset/herança.
- Controles mostram o valor apropriado ao escopo selecionado.
- Preferências existentes possuem efeito consistente e testado.

### Escopo

- **Inclui:** shell moderno, correções do modelo atual, testes e fluxo E2E.
- **Não inclui:** preferências avulsas do layout, editor semântico, secrets,
  integrações do runtime, deployments server-backed ou build fullstack.

### Abordagem

- Centralizar metadados pesquisáveis em `settings.ts` e manter renderizadores de
  controles no dialog.
- Modelar `unset` como remoção da chave no escopo, podando grupos vazios.
- Restringir Theme a User enquanto o provider persistir globalmente.
- Remover Auto Preview da superfície enquanto não existir preview de draft que
  preserve a regra de deploy explícito.

### Riscos e dependências

- Auto Preview não deve recarregar a cada tecla nem alterar a versão implantada.
- Word Wrap precisa manter highlight, caret e gutter alinhados.
- Migração deve preservar valores já armazenados e o worktree em andamento.

## Acceptance criteria

- [x] Busca encontra configuração por título, descrição, ID e keywords.
- [x] Menu lateral navega por Editor, Workbench, Preview & Logs e Files.
- [x] Filtro Modified mostra somente overrides do escopo ativo.
- [x] Reset Workspace herda User; reset User herda default.
- [x] A tab User nunca mostra um override Workspace como valor editável.
- [x] Auto Preview fictício não é exibido; Word Wrap e Files Exclude têm efeito coerente.
- [x] Theme é claramente User-only e não vaza de Workspace.
- [x] Dialog permanece utilizável em viewport compacto.

## Test-first plan

- **Comportamentos:** valor por escopo, remoção de override, filtro pesquisável e
  escopos permitidos.
- **Primeiro teste falhando:** reset remove a chave e poda o grupo vazio.
- **Nível preferido:** unit para modelo, integração de componente para interação
  crítica e E2E manual/catalogado para browser.
- **Valor frontend:** protege persistência local, precedência e efeito real das
  preferências.
- **Evitar:** snapshots de markup e testes que apenas confirmam texto estático.

## Tasks

- [x] Mapear impacto e validar arquivos.
- [x] Escrever testes falhando do modelo. **Done when:** Vitest falha pelos novos contratos.
- [x] Implementar reset, valor por escopo e catálogo. **Done when:** testes unitários passam.
- [x] Construir shell pesquisável e categorizado. **Done when:** busca, lateral e Modified funcionam no browser.
- [x] Corrigir efeitos das preferências. **Done when:** testes e validação manual comprovam comportamento.
- [x] Adicionar fluxo E2E e atualizar documentação. **Done when:** gate de flows passa.
- [x] Executar gates completos e revisar consistência final.
- [x] Alinhar a hierarquia visual à referência. **Done when:** User/Workspace fica no header e busca + Modified ficam na sidebar.

## Verification

- [x] `cd workers/core/webide && bun run test`
- [x] `cd workers/core/webide && bun run typecheck`
- [x] `cd workers/core/webide && bun run build`
- [x] `cd workers/core/webide && bun run test:flows`
- [x] `planning/edger/scripts/webide-ui-gate.sh`
- [x] `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check`
- [x] Browser: User/Workspace, busca e reset; unit tests: Modified, herança e efeitos do modelo.

## Status

completed (2026-07-15) — modelo, UI, efeitos, fluxo E2E catalogado, interações
críticas no Browser builtin e gates do workspace validados. O build permanece
verde com avisos não bloqueantes de migração do Vite/Rolldown e chunk principal
acima de 500 kB.
