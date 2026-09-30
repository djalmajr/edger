# Consolidação dos worktrees — 30/09/2026

## Autorização e escopo
Operador autorizou "faça merge de tudo" após identificar trabalho de produto ainda sem commit. Base origin/main `17ec351`. Consolidação em `fix/cpanel-act-warnings`, sem alteração de versões, release, tag ou deploy.

## Cobertura dos worktrees
- `api-key-permission-edit`: backend e contrato PATCH integrados na rc.6; versões antigas de UI/testes substituídas pela implementação consolidada. Comentário Rust em inglês mantido. Flags de runtime preservadas.
- `fix-cpanel-locale-login`: código integrado pelo PR #69 (squash); somente symlinks locais de dependências fora do versionamento.
- `rc6-release-evidence`: documentação integrada pelo PR #70 (squash).
- `prepare-0.3.2`: CHANGELOG e preparação final incorporados; versões antigas de ajuda/API Keys substituídas pelas emendas acessíveis e de DataGrid.
- `cpanel-act-warnings`: conjunto final de correções de testes, DataGrid de Usuários, ações compactas, ajuda por tooltip, visibilidade persistida de colunas e tooltip do badge de Workers.

Cópias antigas e configurações locais são preservadas nos worktrees; não fazem parte do PR. WebIDE não tem mudança de source: dist ausente foi gerado somente na prévia local.

## Verificações reutilizadas
Hashes SHA-256 dos fontes iguais aos relatórios Sonnet finais:
main `1fc7a78e71740328`; i18n `a9d5c36ff11ad487`; DataGrid `16df7199f1c909aa`; API Keys `c1e7c0b5c756b1de`; Usuários `46fc5fb2f898c1bc`; visibilidade `0cacf4e543cd0e2b`.

Revisões `review-cpanel-claude-20260930T073752.md` e `review-cpanel-claude-20260930T074933.md`: PASS, zero achados; P2 de mocks Bun resolvido. Relatórios no scratch Herdr wF. Gate final: 174 Bun, 157 Vitest, typecheck e build aprovados. Browser da página executado pelo orquestrador; detalhes em `columns-webide-preview-2026-09-30.md` e `cpanel-datagrid-tooltip-icons-2026-09-29.md`.

Rust gate completo executado com cargo +1.98.0 na mesma base/código/dependências: test --workspace, clippy --workspace -- -D warnings e fmt -- --check PASS. Logs `/tmp/edger-columns-{rust-tests,clippy,fmt}-20260930.log`. Nenhuma alteração Rust, dependência, manifest ou chart nesta consolidação. As provas são reutilizadas porque os arquivos relevantes não mudaram; CI do PR reexecutará seus próprios gates.

## Limites
Trabalho local consolidado para PR; merge depende de CI verde. Não declara inspeção autenticada remota, PATCH remoto, release final ou deploy. Sem mudança de banco, dependências ou autenticação. Processo local e worktrees preservados.
