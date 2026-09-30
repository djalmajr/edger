# Plano — consolidar worktrees e mesclar em main

## Contexto
Operador autorizou em 30/09/2026: "faça merge de tudo". Consolidar todo trabalho de produto ainda pendente dos worktrees EdgeR, preservando implementações já integradas por squash. Não inclui PRs Dependabot, release final, tag ou deploy.

## Arquivos
Delta de fix/cpanel-act-warnings sobre origin/main: cPanel (DataGrid, Usuários, Chaves, visibilidade, ajuda/tooltip), fixtures/testes, CHANGELOG e planos/evidências. Os demais worktrees são fontes de comparação; não copiar node_modules, dist, caches, configurações locais ou relatórios do Herdr.

## Detalhes
Conservar a versão integrada mais recente dos arquivos sobrepostos. Auditoria pi/Qwen confirma cobertura sem editar; revisões Sonnet já executadas permanecem válidas se hashes não mudarem. Caminho: commit de trabalho → push branch → PR → CI verde → merge em main. Preservar worktrees antigos e servidor local.

## Tarefas
- [ ] Comparar todos os deltas úteis e resolver lacunas, se houver.
- [ ] Registrar revisão e gates válidos, sem repetir suites inalteradas.
- [ ] Commit e PR da consolidação.
- [ ] CI verde e merge em main.
- [ ] Sincronizar referência main local e registrar resultado.

## Verificação
Hashes dos fontes comparados aos relatórios Sonnet finais; gates Rust/testes Bun/Vitest/typecheck/build já executados com a mesma base, código e dependências. Reexecutar apenas docs/whitespace alterados e CI do PR, que executa Rust, frontend, Helm, imagem, secret scan e advisories. Qualquer falha nova exige correção e revisão aplicável antes do merge.

## Decisões
Consolidar alterações úteis, sem mesclar cópias antigas que reverteriam correções. Configurações locais e artefatos continuam fora do PR. Não remover worktrees nesta demanda.
