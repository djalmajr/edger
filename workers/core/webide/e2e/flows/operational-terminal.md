---
id: operational-terminal
name: Usar o terminal operacional seguro
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: adversarial-security-researcher
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point com Admin API funcional
  - A sessão do Browser possui a API key administrativa válida
  - Existe um projeto válido e deployável
---

## User goal

Consultar e operar o projeto por comandos seguros sem obter acesso a shell ou ao
filesystem do host.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto e clique em Terminal** → o console mostra banner de escopo e input Operational command.
2. **preencha `help` e execute** → a saída lista somente help, validate, deploy, preview, files, status e clear.
3. **preencha `files` e execute** → somente os paths do projeto ativo são listados.
4. **preencha `status` e execute** → nome, versão, estado do draft e preview aparecem.
5. **preencha `preview` e execute** → a URL do último deploy ou a ausência de preview é informada.
6. **preencha `validate` e execute** → o resultado informa Valid com nome e versão ou um erro de projeto, sem deploy.
7. **preencha `pwd` e execute** → aparece `Unknown command: pwd. Type help.` e nenhum path do host é exposto.
8. **preencha `ls` e execute** → o comando também é rejeitado como desconhecido.
9. **preencha uma linha vazia e execute** → nenhum item é adicionado ao histórico.
10. **preencha `clear` e execute** → o histórico visual do terminal é removido.
11. **preencha `help` novamente** → o terminal continua funcional após clear.
12. **preencha `deploy` e execute** → a saída informa início do deploy e o footer muda para Deployments.
13. **aguarde o deploy concluir** → etapas e histórico mostram o resultado sem executar shell arbitrário.
14. **volte a Terminal** → o console permanece restrito e pronto para novos comandos permitidos.

## Expected result

Todos os comandos documentados funcionam no escopo do projeto, comandos de host
são rejeitados e deploy continua explícito e observável.
