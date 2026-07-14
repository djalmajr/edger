---
id: log-preservation
name: Controlar preservação dos logs entre deploys
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: edger-platform-operator
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point com Admin API funcional
  - A sessão do Browser possui a API key administrativa válida
  - Existe um projeto válido e deployável
---

## User goal

Escolher se um novo deploy começa com logs limpos ou mantém o histórico local.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto e clique em Logs** → a opção Preserve logs across restarts fica visível.
2. **desmarque Preserve logs across restarts** → o checkbox muda para desmarcado.
3. **clique em Validate project** → um evento VALIDATE de sucesso é adicionado como marcador anterior ao deploy.
4. **clique no ícone Deploy project e aguarde concluir** → os logs anteriores são limpos no início e a lista final contém somente a sequência do novo deploy.
5. **marque Preserve logs across restarts** → a preferência passa a habilitada.
6. **clique em Validate project novamente** → um novo marcador VALIDATE aparece após os logs existentes.
7. **execute outro deploy explícito e aguarde concluir** → o marcador e a sequência anterior permanecem acima dos novos logs.
8. **recarregue a página e abra Logs** → checkbox continua marcado e o histórico preservado é restaurado.
9. **clique em outra tab do footer e volte a Logs** → o estado do checkbox e os eventos não mudam.
10. **desmarque Preserve logs across restarts** → o projeto fica preparado para uma futura execução com limpeza.
11. **recarregue a página** → a preferência desmarcada também persiste.

## Expected result

Desmarcado limpa eventos anteriores no próximo deploy; marcado preserva eventos,
e a preferência sobrevive a reload.
