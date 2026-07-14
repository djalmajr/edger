# Usability — Implantar e operar o preview versionado (deploy-and-preview)

- **Persona:** Operador da plataforma EdgeR · **Date:** 2026-07-14 · **Entry:** http://127.0.0.1:19080/webide
- **Verdict:** ✅ product flow passed; ⚠️ in-app Browser suppresses page-created tabs

## Walkthrough

1. `static-spa-app-6` abriu sem deployment e com o empty state do Preview.
2. Deployments exibiu `No deployments yet.`.
3. O botão do Preview iniciou o deploy explícito; o pipeline local concluiu rápido demais para o runner capturar o estado disabled intermediário.
4. Validation, Packaging, Upload, Release / migrations, Health check, Activation e Complete apareceram em ordem.
5. O histórico registrou Succeeded e o iframe passou a usar `/static-spa-app-6@1.0.0`.
6. Refresh recarregou o mesmo endpoint sem adicionar deployment.
7. `Open in new tab` expôs um link semântico, sem credenciais, para a versão; o backend do Browser suprimiu tanto `target=_blank` quanto Cmd+click. A URL lida do link foi aberta em uma aba isolada pelo runner e renderizou `Hello from EdgeR` sem superfície administrativa.
8. Alterar o manifesto para 1.0.1 manteve o Preview em 1.0.0.
9. Cmd+S e o botão do header iniciaram o segundo pipeline explícito.
10. O histórico mostrou dois registros Succeeded em ordem temporal decrescente.
11. O iframe mudou para `/static-spa-app-6@1.0.1` e seu heading ficou acessível no frame.
12. Reload restaurou projeto, iframe 1.0.1 e os dois registros de deployment.

## Findings (prioritized)

| # | Severity | Step | What happened | Suggested fix |
|---|---|---|---|---|
| 1 | blocker | precondition | A sessão inicial não continha a chave administrativa; Upload falhou com `missing or invalid API key`. | Resolvido na precondição: autenticar pela UI do cPanel com a chave local antes do rerun. |
| 2 | major | 7 | O preview usava `window.open`, que pode ser bloqueado e não oferece destino navegável sem JavaScript. | Resolvido: renderizar link shadcn semântico com `target=_blank`, `rel=noopener noreferrer` e `aria-label`. |
| 3 | runner limitation | 7 | O in-app Browser não cria tabs disparadas pela página, inclusive para link semântico e Cmd+click. | Evidência alternativa: abrir em tab isolada a URL obtida do próprio link e validar conteúdo e ausência da sessão administrativa. |

## Rerun

O fluxo completo foi repetido em projeto novo e sessão autenticada após o build. Os deployments 1.0.0 e 1.0.1, preview isolado e restauração passaram; somente a materialização da aba externa permaneceu limitada pelo runner.

## Key screens

- [Preview 1.0.1 e histórico restaurado](screenshots/2026-07-14/deploy-and-preview/preview-1.0.1-history.png)
