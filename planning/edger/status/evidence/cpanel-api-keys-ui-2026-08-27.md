# Walkthrough da tela API Keys — 2026-08-27

Chromium headless contra a instância local (0.3.0, porta 19080). Reexecutável:

    node planning/edger/scripts/cpanel-keys-walkthrough.mjs

```
1) login com a root key
  PASS  entrou no painel
2) a aba API Keys existe e navega
  PASS  item de navegação visível
  PASS  rota /cpanel/keys — http://localhost:19080/cpanel/keys
  PASS  tabela reflete a API — ui=1 api=1
3) o diálogo oferece o catálogo inteiro e se protege
  PASS  catálogo com as 7 permissions
  PASS  workers:read vem marcado por default
  PASS  Create bloqueado sem nome
4) a guarda do formulário responde ao estado
  PASS  Create libera com nome + permission default
  PASS  rótulo alterna a caixa
  PASS  Create volta a bloquear sem nenhuma permission
  PASS  Create libera de novo
5) criar com escopo restrito
6) o segredo aparece UMA vez
  PASS  raw key no painel one-time
  PASS  avisa que não será mostrada de novo
7) a credencial vale de verdade, e só até onde deve
  PASS  autentica no admin — status=200
  PASS  enxerga só o escopo chunked-* — chunked-text
  PASS  não gerencia keys (sem keys:manage) — status=403
8) fechar o painel e ver a linha nova
  PASS  linha nova na tabela — antes=1 depois=2
  PASS  mostra a permission concedida
  PASS  mostra o escopo de worker
9) apagar confirma e some da lista
  PASS  achou a linha criada
  PASS  pede confirmação
  PASS  diz que é irreversível — Delete key  Delete “walkthrough-ui”? The credential stops working imme
  PASS  linha sumiu da tela — voltou para 1
  PASS  apagada no SERVIDOR, não só na tela
  PASS  sem erro de JS no console

TOTAL: 25 passaram, 0 falharam
```
