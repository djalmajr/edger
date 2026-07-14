---
id: footer-panels-and-order
name: Usar e ordenar os painéis inferiores
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: edger-platform-operator
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto com logs e ao menos um deployment
---

## User goal

Alternar rapidamente entre diagnóstico, eventos, console e histórico, mantendo a
organização preferida.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto preparado** → o footer restaura visibilidade, tab selecionada e ordem anterior.
2. **clique em Problems** → o painel mostra diagnósticos do arquivo ativo ou o empty state sem problemas.
3. **clique em Logs** → eventos exibem horário, source, nível e mensagem; Preserve logs aparece somente nessa tab.
4. **clique em Terminal** → o banner explica que o console é operacional e não um host shell.
5. **clique em Deployments** → etapas recentes e histórico local aparecem.
6. **arraste Deployments para antes de Problems** → o indicador de destino aparece e a ordem muda ao soltar.
7. **arraste Logs para depois de Terminal** → a segunda ordem é aplicada sem trocar o conteúdo dos painéis.
8. **clique no ícone Hide panel do header** → footer e splitter horizontal desaparecem.
9. **clique em Show panel e selecione Logs** → o painel retorna, abre a tab escolhida e mantém a ordem reconfigurada.
10. **recarregue a página** → visibilidade, tab atual e ordem personalizada persistem.

## Expected result

As quatro tabs exibem seus conteúdos, podem ser reordenadas e restauram ordem e
visibilidade após reload.
