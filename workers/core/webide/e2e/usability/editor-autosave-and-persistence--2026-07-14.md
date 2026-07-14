# Usability — Editar e restaurar um rascunho local (editor-autosave-and-persistence)

- **Persona:** Engenheira de confiabilidade de releases · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ completable

## Walkthrough

1. `index.html` abriu com editor, gutter de seis linhas e syntax layer.
2. Tab no início da linha vazia final inseriu exatamente dois espaços sem perder sincronização.
3. `<p id="ux-flow">Autosave flow</p>` foi inserido dentro do body e o highlighting acompanhou.
4. Após o debounce, o rascunho persistiu sem ação de deploy.
5. Cmd+S concluiu o salvamento local.
6. Alternar para `manifest.yaml` e voltar preservou a edição.
7. Reload restaurou projeto, `index.html`, ordem de tabs, parágrafo e indentação.
8. Deployments continuou com os dois sucessos preexistentes e nenhuma entrada nova.
9. Preview permaneceu em `/static-spa-app-7@1.0.1`.
10. O conteúdo original foi restaurado e salvo para os fluxos seguintes.

## Findings (prioritized)

Nenhum finding de produto neste fluxo.

## Key screens

- [Conteúdo original restaurado após a prova de autosave](screenshots/2026-07-14/editor-autosave-and-persistence/original-restored.png)
