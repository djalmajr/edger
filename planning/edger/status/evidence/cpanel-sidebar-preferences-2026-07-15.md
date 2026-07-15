# Evidência — preferências e conta no header do cPanel

Data: 2026-07-15

## Implementação

- Provider i18n local com `pt-BR`, `en-US` e `es-ES`, persistência e atualização
  de `document.lang`.
- Menu de tema reutiliza `@edger/ui/lib/theme` para claro, escuro e sistema.
- Avatar apresenta nome, perfil, namespaces e logout sem duplicar esses dados no
  header.
- Idioma, tema e avatar ocupam o canto direito do header; a sidebar fica
  dedicada à navegação.

## TDD

- Red: `bun test src/lib/i18n.test.ts` falhou com `Cannot find module './i18n'`.
- Green: normalização, fallback e traduções do shell passaram.

## Browser

- Idioma mudou de inglês para português, atualizando navegação, controles e
  `html[lang]` para `pt-BR`.
- Tema escuro aplicou a classe `dark` e atualizou o controle para
  `Tema: Escuro`.
- Avatar `RO` abriu popover com `root`, `Perfil: admin`, `Namespaces: *` e
  `Sair`.
- `Sair` removeu a sessão e retornou à tela de conexão.

## Gates

- `bun test`: 9 testes passaram.
- `bun run build`: passou.
- `planning/edger/scripts/cpanel-ui-gate.sh`: passou.
- Refinamento: `0 RED`.
- `cargo test --workspace`: passou.
- `cargo clippy --workspace -- -D warnings`: passou.
- `cargo fmt -- --check`: passou.
- `git diff --check`: passou.
