---
id: project-search
name: Buscar conteúdo em todo o projeto
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: webide-power-developer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto com ocorrências de Hello e hello em arquivos diferentes
---

## User goal

Localizar rapidamente texto ou padrões em todos os arquivos e abrir a linha
correta no editor.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto preparado** → o workbench abre com Explorer ativo.
2. **clique no ícone Search da activity bar** → o painel SEARCH abre e o input recebe foco.
3. **preencha `hello`** → resultados agrupados por arquivo mostram total, número da linha, trecho e destaque do match.
4. **clique em Match case** → somente ocorrências com a mesma capitalização permanecem e `aria-pressed` reflete o estado.
5. **altere a consulta para `Hello`** → os resultados case-sensitive correspondentes aparecem.
6. **clique em Use regular expression e preencha `H[a-z]+o`** → a busca regex retorna matches compatíveis.
7. **preencha uma expressão inválida `[`** → o resumo mostra `Invalid regular expression` sem travar o workbench.
8. **desative regex e case-sensitive e preencha `definitely-not-present`** → o resumo mostra `No results.` sem grupos vazios.
9. **preencha novamente `hello`** → a busca simples volta a funcionar.
10. **clique em um resultado de outro arquivo** → o arquivo abre em tab e o cursor é posicionado na linha informada.
11. **pressione Ctrl/Cmd+Shift+F a partir do editor** → SEARCH volta a abrir com o input focado.
12. **clique no ícone Explorer** → a árvore retorna e o arquivo localizado continua selecionado.

## Expected result

Busca simples, case-sensitive, regex, erro de regex, shortcut e navegação para a
linha funcionam sem perder seleção ou estado do projeto.
