# Story 21.15: Preferências e conta no header

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problema:** preferências e conta são controles globais, mas estavam
  confinados ao rodapé da sidebar e competiam com a navegação lateral.
- **Objetivo:** alinhar o header ao padrão já validado no appliance, com
  preferências compactas e conta acessível por avatar no canto direito.
- **Restrição:** usar a sessão e o ThemeProvider existentes; não criar endpoint
  de perfil nem autenticação paralela.

## Scope

- **In:** i18n persistido para o shell autenticado, tema claro/escuro/sistema,
  avatar com informações do principal e logout.
- **Out:** tradução integral das páginas operacionais e edição do perfil.

## Files

| Path | Action | Reason |
|---|---|---|
| `workers/core/cpanel/src/lib/i18n.tsx` | add | Provider e catálogo do shell |
| `workers/core/cpanel/src/main.tsx` | edit | Menus e composição do header |
| `workers/ui/src/icons/lucide.ts` | edit | Ícones de tema compartilhados |
| `planning/edger/scripts/cpanel-ui-gate.sh` | edit | Proteger os novos contratos |

## Detail

- O locale inicial usa preferência persistida ou idioma do navegador, com
  fallback seguro para português.
- Tema continua sendo uma preferência puramente cliente do provider comum.
- O menu da conta deriva todos os dados de `RuntimeData.principal` e o logout
  remove somente a credencial mantida em `sessionStorage`.

## Acceptance criteria

- [x] Idioma oferece português, inglês e espanhol e persiste localmente.
- [x] Navegação, títulos do shell e controles de conta respondem ao idioma.
- [x] Tema oferece claro, escuro e sistema usando o provider compartilhado.
- [x] Avatar abre menu com nome, perfil, namespaces e ação de logout.
- [x] Header não repete nome/perfil fora do menu da conta.
- [x] Sidebar fica dedicada à marca e à navegação.

## Test-first plan

- **Red:** teste de normalização/tradução falha antes do módulo i18n existir.
- **Green:** provider, menus e composição compacta do header.
- **Refactor:** manter preferências independentes do fluxo de autenticação.

## Tasks

- [x] Criar provider i18n e testes de normalização/tradução.
- [x] Traduzir navegação, títulos e controles do shell autenticado.
- [x] Adicionar menus compactos de idioma e tema.
- [x] Substituir identidade textual pelo avatar com menu da conta.
- [x] Validar logout, persistência, tema e locale no Browser.

## Verification

```bash
cd workers/core/cpanel && bun test && bun run build
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed** (2026-07-15) — implementação, gates e prova Browser registrados
em `planning/edger/status/evidence/cpanel-sidebar-preferences-2026-07-15.md`.
