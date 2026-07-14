---
id: dashboard-project-lifecycle
name: Duplicar, renomear e excluir projetos locais
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: webide-power-developer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto local chamado webide-spa-e2e
---

## User goal

Gerenciar o ciclo de vida dos rascunhos locais sem afetar workers implantados.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **passe o ponteiro sobre a linha `webide-spa-e2e`** → apenas o background da linha inteira muda e o texto mantém a mesma cor.
2. **Clique em Duplicate project na linha** → uma nova linha `webide-spa-e2e-copy` aparece sem abrir o projeto original.
3. **clique em Rename project na cópia e cancele o prompt** → o nome permanece `webide-spa-e2e-copy`.
4. **clique novamente em Rename project, preencha `webide-flow-renamed` e confirme** → o nome da linha é atualizado.
5. **clique na linha da cópia, abra `manifest.yaml` e observe Preview e Deployments** → o manifesto usa o novo name, arquivos foram copiados e preview/deployments começaram vazios.
6. **clique no logo para voltar ao dashboard, recarregue e pesquise `webide-flow-renamed`** → o projeto renomeado continua persistido.
7. **clique em Delete project na cópia** → uma confirmação avisa que somente o projeto local será excluído.
8. **cancele a confirmação** → a cópia permanece na tabela.
9. **clique novamente em Delete project e confirme** → a cópia desaparece e o projeto original permanece.
10. **limpe a busca** → o inventário completo reaparece ordenado por atualização recente.
11. **clique na célula Updated do projeto original** → o workbench abre, provando que as ações de linha não quebraram sua navegação.
12. **volte ao dashboard** → somente os projetos preservados aparecem e nenhuma ação alterou deployments remotos.

## Expected result

Duplicate, Rename e Delete operam sobre o rascunho correto, são independentes do
link da linha e persistem somente as mudanças confirmadas.
