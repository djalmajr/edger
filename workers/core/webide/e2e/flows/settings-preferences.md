---
id: settings-preferences
name: Pesquisar, alterar e restaurar preferências por escopo
reference: planning/edger/epics/22-core-workers-webide/09-settings-modernas.md
persona: assistive-technology-developer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto com ao menos um arquivo e Workspace Settings disponível
---

## User goal

Localizar preferências, compreender User/Workspace e restaurar herança sem editar
armazenamento local manualmente.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra um projeto e ative Settings pela activity rail** → o dialog abre com User/Workspace no header e busca, Modified e categorias na sidebar, com Editor selecionado.
2. **preencha Search settings com `long lines`** → somente Word Wrap aparece e o ID `editor.wordWrap` permanece visível.
3. **limpe a busca, ative Workspace e altere Font Size** → a linha informa `Modified in Workspace` e o editor aplica o novo tamanho.
4. **ative Modified** → somente preferências alteradas no escopo Workspace aparecem.
5. **ative Reset Font Size** → o override é removido, o valor volta a herdar User/default e o filtro Modified mostra estado vazio.
6. **volte para User, abra Workbench e altere Theme** → o tema muda e a categoria não aparece em Workspace.
7. **abra Files, adicione um padrão que exclua um arquivo conhecido e feche Settings** → o arquivo desaparece de Explorer e Search.
8. **reabra Settings, pesquise `exclude` e remova ou resete o padrão** → o arquivo volta a aparecer nas duas superfícies.
9. **ative Word Wrap e abra uma linha longa** → texto visível e campo editável quebram linhas juntos sem sobreposição do gutter.
10. **reduza o viewport e navegue pelas categorias** → busca, escopos, Modified e categorias permanecem operáveis sem scroll horizontal estrutural.

## Expected result

Settings permite descoberta, edição por escopo, indicação de origem e reset; cada
preferência exibida possui efeito observável e o layout permanece acessível.
