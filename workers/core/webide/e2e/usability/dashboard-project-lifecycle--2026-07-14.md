# Usability — Duplicar, renomear e excluir projetos locais (dashboard-project-lifecycle)

- **Persona:** Desenvolvedor frequente da WebIDE · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable after fix

## Walkthrough

1. A linha inteira de `webide-spa-e2e` preservou a cor do texto; a regra de hover está aplicada somente ao background da row.
2. `Duplicate project` criou `webide-spa-e2e-copy` sem abrir o original.
3. O primeiro `Rename project` abriu um Dialog shadcn; `Cancel` preservou o nome da cópia.
4. O segundo rename para `webide-flow-renamed` atualizou a linha.
5. A cópia abriu com os arquivos preservados, manifesto renomeado, preview vazio e `No deployments yet.`.
6. O retorno, reload e busca confirmaram a persistência do rename.
7. `Delete project` abriu um AlertDialog shadcn explicando que somente o rascunho local seria removido e deployments não seriam afetados.
8. `Cancel` preservou a cópia.
9. A segunda confirmação removeu somente `webide-flow-renamed`.
10. A limpeza da busca restaurou o inventário completo, com `webide-spa-e2e` preservado e ordenado primeiro por atualização.
11. Um clique na célula Updated abriu o workbench original.
12. O logo retornou ao dashboard sem recriar a cópia nem alterar o projeto original.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | blocker | 3 | `Rename project` usava `prompt()`, não suportado pelo Browser embutido; o clique não mostrava UI e gerava erro no console. | Resolvido: substituir prompts por Dialog shadcn, confirmações por AlertDialog e feedback imperativo por Sonner. |

## Rerun

O fluxo completo foi repetido desde o entry point após o build. Os 12 passos passaram com Dialog, AlertDialog, foco inicial e cancelamento/confirmacão funcionando.

## Key screens

- [Dashboard e linha do projeto base](screenshots/2026-07-14/dashboard-project-lifecycle/01-row-hover.png)
- [Dashboard final com original preservado](screenshots/2026-07-14/dashboard-project-lifecycle/02-final-dashboard.png)
