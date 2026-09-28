# Story 26.02: tela de acesso e favicon

**Origin:** `planning/edger/epics/26-console-auth/00-overview.md`; screenshot do operador de 2026-09-27 e componente `AdminAccessScreen` do Apigate como referência visual.

## Context

O formulário atual pede apenas root key. O operador quer a mesma hierarquia visual do login do appliance e uma alternativa por token na mesma tela.

## Files

`workers/core/cpanel/src/main.tsx`, `lib/api.ts`, `lib/i18n.tsx`, testes, `src/index.html` e `src/favicon.svg`.

## Detail

Card central, ícone de usuários, título “Acesso administrativo”, idioma/tema, campos usuário/senha com botão de visibilidade, “Entrar” primário e “Entrar com token” recolhido. A cópia sobre root semeado só aparece quando a API confirma; caso contrário a UI mostra caminho de token. Login por senha guarda sessão na aba; login por token preserva o fluxo existente. Logout revoga sessão no servidor e limpa estado local. Favicon vetorial original usa a paleta do EdgeR e href relativo ao Static SPA.

### Acceptance criteria

- Layout responsivo reproduz a referência em pt-BR e mantém en-US/es-ES.
- Senha e token funcionam, 401/429/503 são distinguíveis e não aparecem em URL/log.
- Favicon responde com MIME SVG no caminho servido pelo EdgeR e é legível em 16/32 px.

## Tasks

- [x] Implementar formulário, fallback por token, estados de erro e logout.
- [x] Adicionar traduções e testes da API/DOM.
- [x] Criar favicon, buildar SPA e validar visualmente no Browser local.

## Verification

```bash
cd workers/core/cpanel && bun test && bun run build
bash planning/edger/scripts/cpanel-ui-gate.sh
```

Registrar screenshot e testes em `planning/edger/status/evidence/console-auth-2026-09-27.md`.
