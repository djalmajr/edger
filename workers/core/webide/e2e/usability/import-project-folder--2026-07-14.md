# Usability — Importar um projeto a partir de pasta local (import-project-folder)

- **Persona:** Autor iniciante no EdgeR WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ❌ blocked at "step 1"

## Walkthrough

1. `Import` abriu o seletor de pasta, mas o backend do Codex In-app Browser recusou o upload antes de entregar qualquer fixture à WebIDE. O Chrome foi verificado como instalado, em execução, com extensão habilitada e native host correto, porém a conexão de automação permaneceu indisponível.
2. Não executado; a aplicação não chegou a receber a pasta inválida.
3. Não executado.
4. Não executado.
5. Não executado.
6. Não executado.
7. Não executado.
8. Não executado.
9. Não executado.
10. Não executado.
11. Não executado.
12. Não executado.
13. Não executado.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | blocker | 1 | O Codex In-app Browser não suporta upload de pasta e a conexão do Chrome não ficou disponível, impedindo selecionar a fixture. A limitação ocorre antes do código EdgeR receber arquivos. | Restabelecer a conexão do ChatGPT Chrome Extension e reexecutar o fluxo completo no Chrome; não alterar a WebIDE para contornar uma limitação do executor. |

## Key screens

Não há tela interna da WebIDE capaz de representar o bloqueio: ele ocorre no seletor nativo de pasta, antes da importação.

## Rerun

Pendente. O fluxo não pode ser considerado verificado até os 13 passos passarem em um navegador com suporte a upload de diretório.
