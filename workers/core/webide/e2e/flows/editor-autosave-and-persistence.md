---
id: editor-autosave-and-persistence
name: Editar e restaurar um rascunho local
reference: planning/edger/epics/22-core-workers-webide/05-webide-editor-drafts.md
persona: release-reliability-engineer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto descartável com index.html
---

## User goal

Editar código com feedback imediato e confiar que o rascunho sobreviverá sem
realizar deploy.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto descartável e selecione `index.html`** → editor, gutter e syntax highlighting mostram o conteúdo atual.
2. **posicione o cursor no início de uma linha e pressione Tab** → dois espaços são inseridos e gutter/highlight continuam sincronizados.
3. **adicione o texto `<p id="ux-flow">Autosave flow</p>` dentro do body** → o editor atualiza o highlighting e agenda o autosave.
4. **aguarde o intervalo de autosave sem clicar em Deploy** → o rascunho é salvo localmente e nenhum histórico de deployment é criado.
5. **pressione Ctrl/Cmd+S** → o salvamento local é concluído imediatamente sem chamar instalação administrativa.
6. **clique em outra tab e volte para `index.html`** → a edição continua presente.
7. **recarregue a página** → o mesmo projeto, tab ativa, ordem de tabs e texto editado são restaurados do IndexedDB.
8. **clique em Deployments no footer** → não existe novo deployment causado pelo autosave ou Ctrl/Cmd+S.
9. **clique em Preview** ou restaure o painel lateral se oculto → o preview continua apontando somente para a última versão implantada, não para o rascunho.
10. **remova o parágrafo temporário e pressione Ctrl/Cmd+S** → o conteúdo original volta a ficar persistido para os demais fluxos.

## Expected result

Edição, indentação, highlight, autosave, salvamento explícito local e restauração
funcionam sem criar deploy nem alterar o preview ativo.
