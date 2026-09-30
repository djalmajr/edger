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

## Auditoria de cobertura

pi/Qwen concluiu comparação de 18 arquivos pendentes em api-key-permission-edit, sete em prepare-0.3.2 e os 23 arquivos do commit consolidado. Nenhuma alteração útil ausente. Relatório `cpanel-qwen-20260930T094745.md` no scratch Herdr wF.

Dois itens foram marcados partial pelo auditor por remoção dos parágrafos keys.lead/users.lead do corpo. Fechados pelo orquestrador: remoção intencional, pedida pelo operador ao mover ajuda para tooltip. Os dois textos estão preservados em PageTitleHelp no título, com escolha keys/users em main.tsx; planos cpanel-datagrid-pins-20260929.md e prova browser correspondente documentam a decisão. Restaurar os parágrafos duplicaria a ajuda e reverteria o pedido. Nenhum patch adicional necessário.

## Bloqueio novo no CI

PR #71 aberto com commit 255162a. CI inicial passou Rust, OTLP, frontend/planejamento, Helm e secret scan. Advisories falhou por RUSTSEC-2026-0316 (wasmtime36.0.15) e RUSTSEC-2026-0314 (wasmtime-wasi36.0.15). Correção mínima em curso: patch36.0.16 da mesma major, conforme avisos oficiais GHSA-jqpg-j7w6-42pr e GHSA-j2g9-4prp-pf6h. Não ignorar advisories. Provas Rust anteriores não cobrem essa atualização e serão reexecutadas após o lockfile final.

## Fechamento das emendas antes do merge

Wasmtime36.0.16: autor release-qwen-20260930T100357.md; revisão independente review-cpanel-claude-20260930T100952.md PASS zero achados. cargo-deny advisories/licenses OK sem ignores. Mesmas features e listas de dependências; 23 pacotes36.0.16 e13Cranelift0.123.16. Gate Rust completo reexecutado e PASS, logs /tmp/edger-pr71-{rust-tests,clippy,fmt}.log. Avisos cargo-deny spin yanked e saffron license são preexistentes, reproduzidos também no baseline.

Pins adicionais: trigger no badge; política oculta quando ambas flags off (pin6dd70b75-3734-453c-aac5-4f07258eb562/capturef34ea9a1-7697-4431-9ce1-c93c3d69b8c9); logs multi (pinc4044d69-028a-4c24-80c5-7a9a6b317ee1/capture226190a6-b6a3-4983-b278-b17183395660); buscaUsers/Keys (pin3dd421ff-4a0a-41a1-acf7-4c335c6fbb84/capturea05df59f-08a9-41e0-9d46-9005fb7d342d).

Autor Workers/Logs cpanel-qwen-20260930T101408.md; revisão review-cpanel-claude-20260930T103244.md PASS0. Autor busca release-qwen-20260930T101236.md e emenda103238; revisão review-cpanel-claude-20260930T103601.md PASS0. Resultado final176 Bun/159 Vitest/typecheck PASS. O partial de Bun da busca foi fechado: executar a partir de workers passa; a falha101/58 era execução a partir de cpanel com descoberta/harness diferente. Fixtures não foram alteradas para contornar isso.

Browser real após build final (/tmp/edger-pr71-cpanel-build.log):
- Badge span foco/hover fora do botão, aria-describedby/id iguais. Hover no nome não abre tooltip. Badge x1292..1333; tooltip x1087..1355 e y245, junto ao canto direito. Escape fecha; botão expande/recolhe normalmente, sem interativo aninhado.
- Expandir WebIDE com flags reais off: tabela aparece, política ausente e zero chamadas routing-policy.
- BuscaUsers: página2 (2 linhas)→root (1 linha, próxima página disabled); inexistente mostra noResults, limpar restaura15 linhas. Keys: busca inexistente mostra noResults, limpar restaura lista. Dados locais sintéticos; nenhum usuário/chave alterado.
- Logs: respostas HTTP interceptadas apenas no browser com35 eventos sintéticos (sem persistência/alteração de backend). Warning+Error elimina info; combinado com disk exibe12 eventos esperados; removerWarning deixaError e desmarcarError retornaAll levels. Página reseta da segunda para primeira ao mudar filtro. Interceptação removida e página recarregada com API real ao fim.

Screenshots em /Users/djalmajr/Developer/djalmajr/edger/output/playwright/: edger-badge-corner-20260930.png, edger-search-keys-20260930.png, edger-log-multiselect-20260930.png. Browser não é prova de produção.
