---
id: explorer-file-folder-lifecycle
name: Organizar arquivos e pastas pelo Explorer
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: webide-power-developer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto descartável para o fluxo
design_refs:
  explorer: "planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md"
---

## User goal

Criar e reorganizar uma árvore de projeto completa usando ações visíveis e menu
de contexto.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto descartável** → o workbench abre com Explorer ativo.
2. No header do Explorer, **clique em New folder, preencha `src` e confirme** → a pasta vazia aparece na árvore.
3. **clique no menu de reticências de `src` e escolha New folder**; preencha `components` e confirme → `src/components` aparece com guia de nesting.
4. **clique no menu de `components`, escolha New file, preencha `button.ts` e confirme** → `src/components/button.ts` é criado, selecionado e aberto em uma tab.
5. **clique em New file no header, preencha `notes.md` e confirme** → um arquivo raiz é criado com ícone Markdown.
6. **clique em New file novamente, preencha `notes.md` e confirme** → o dialog permanece aberto e informa `Path already exists: notes.md`.
7. **clique em Cancel, abra New file, preencha `../escape.js` e confirme** → o dialog rejeita o path inválido e nenhum arquivo escapa da raiz do projeto.
8. **clique em Cancel** → o erro fecha sem alterar a árvore.
9. **clique na pasta `src`** → seus filhos são recolhidos; clique novamente → nesting e descendentes reaparecem.
10. **clique com o botão direito em `src/components/button.ts`** → o mesmo menu de contexto oferece Rename e Delete.
11. **clique fora do menu e abra-o novamente pelas reticências** → o menu fecha ao perder contexto e oferece as mesmas ações pelos dois gatilhos.
12. **escolha Rename, preencha `button-renamed.ts` e confirme** → path, tab aberta e seleção passam a usar o novo nome.
13. **clique no menu de `components`, escolha Rename, preencha `ui` e confirme** → pasta, arquivo descendente, tab e seleção são movidos para `src/ui`.
14. **clique no menu de `notes.md`, escolha Delete e cancele** → o arquivo continua na árvore.
15. **repita Delete e confirme** → `notes.md` e sua tab são removidos.
16. **clique no menu de `src`, escolha Delete e confirme** → pasta, descendentes, tabs e seleção relacionados são removidos.
17. **recarregue a página** → as exclusões persistem e nenhum path órfão reaparece.

## Expected result

Criação, nesting, collapse, conflito, rename e delete mantêm árvore, tabs,
seleção e armazenamento local consistentes.
