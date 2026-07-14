---
id: lint-and-problems
name: Detectar e navegar por problemas locais
reference: planning/edger/epics/22-core-workers-webide/08-reference-workbench-layout.md
persona: webide-power-developer
entry: "http://127.0.0.1:19080/webide"
preconditions:
  - EdgeR em execução no entry point
  - Existe um projeto descartável
---

## User goal

Receber diagnóstico básico enquanto edita e saltar do painel Problems para a
linha problemática.

## Steps (each step is a UI ACTION + the expected result)

1. No entry point, **abra o projeto descartável e clique em Problems** → o painel informa que não há problemas no arquivo válido ativo.
2. No Explorer, **crie `broken.json`, abra-o e preencha `{ "value": }`** → Problems mostra erro de JSON com arquivo e linha.
3. **clique no problema de JSON** → o editor recebe foco na linha indicada.
4. **crie `broken.ts` e preencha `export function flow( {`** → o lint indica delimitadores não fechados.
5. **clique no problema de TypeScript** → o cursor salta para a linha do delimitador.
6. **crie `broken.html` e preencha `<html><body><h1>Flow`** → Problems lista tags HTML não fechadas.
7. **crie `broken.yaml` e insira uma linha indentada com Tab** → Problems informa que YAML deve usar espaços.
8. **abra `manifest.yaml` e remova temporariamente `version`** → Problems informa o campo obrigatório ausente.
9. **restaure `version` e altere entrypoint para `missing.js`** → Problems informa `Entrypoint not found: missing.js`.
10. **restaure o manifesto válido** → os diagnósticos de manifesto desaparecem sem deploy.
11. **corrija ou exclua `broken.json`, `broken.ts`, `broken.html` e `broken.yaml` pelos menus do Explorer** → cada diagnóstico some quando seu arquivo deixa de ser inválido.
12. **reabra Problems no arquivo válido** → o estado final informa `No problems detected in the active file.`.

## Expected result

Lint de JSON, JS/TS, HTML, YAML e manifesto aparece durante a edição, navega para
a linha e desaparece após correção ou exclusão.
