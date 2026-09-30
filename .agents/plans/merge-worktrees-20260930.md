# Plano — consolidar worktrees e mesclar em main

## Contexto
Operador autorizou em 30/09/2026: "faça merge de tudo". Consolidar todo trabalho de produto ainda pendente dos worktrees EdgeR, preservando implementações já integradas por squash. Não inclui PRs Dependabot, release final, tag ou deploy.

## Arquivos
Delta de fix/cpanel-act-warnings sobre origin/main: cPanel (DataGrid, Usuários, Chaves, visibilidade, ajuda/tooltip), fixtures/testes, CHANGELOG e planos/evidências. Os demais worktrees são fontes de comparação; não copiar node_modules, dist, caches, configurações locais ou relatórios do Herdr.

## Detalhes
Conservar a versão integrada mais recente dos arquivos sobrepostos. Auditoria pi/Qwen confirma cobertura sem editar; revisões Sonnet já executadas permanecem válidas se hashes não mudarem. Caminho: commit de trabalho → push branch → PR → CI verde → merge em main. Preservar worktrees antigos e servidor local.

## Tarefas
- [x] Comparar todos os deltas úteis e resolver lacunas, se houver.
- [x] Registrar revisão e gates válidos, sem repetir suites inalteradas.
- [x] Commit e PR da consolidação.
- [ ] CI verde e merge em main.
- [ ] Sincronizar referência main local e registrar resultado.

## Verificação
Hashes dos fontes comparados aos relatórios Sonnet finais; gates Rust/testes Bun/Vitest/typecheck/build já executados com a mesma base, código e dependências. Reexecutar apenas docs/whitespace alterados e CI do PR, que executa Rust, frontend, Helm, imagem, secret scan e advisories. Qualquer falha nova exige correção e revisão aplicável antes do merge.

## Decisões
Consolidar alterações úteis, sem mesclar cópias antigas que reverteriam correções. Configurações locais e artefatos continuam fora do PR. Não remover worktrees nesta demanda.

## Emenda de dependências

CI inicial do PR #71 detectou advisories na família Wasmtime36.0.15. Aplicar somente patch36.0.16 em Cargo.toml/lock, revisar e reexecutar gates Rust. A correção é necessária para o merge; não inclui atualização major49 ou outros PRs Dependabot.

## Pins recebidos durante a integração

- [ ] Tooltip do alerta: trigger somente no badge do canto, irmão do botão de expansão; foco/hover próximos, sem interatividade aninhada.
- [ ] Política de roteamento ausente quando ambas flags off; qualquer flag true mantém seu controle correspondente.
- [ ] Logs: combinar níveis info/warn/error em seleção múltipla; vazio mantém todos; combinar com busca e resetar página.
- [ ] Busca local em Usuários e API Keys, traduzida, preservando paginação e ações.
- [ ] Revisão executada Sonnet, build e browser das emendas, antes do merge.

Perguntas respondidas: janela5min é visão rápida, API suporta até15min; recomendação de seletor1/5/15 não implementada por ser pergunta nesta rodada. src aparece na prévia por servir worktree; Docker runtime publica manifest/dist apenas, sem src.
