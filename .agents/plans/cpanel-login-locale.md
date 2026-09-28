# Correções do login e da visão geral do cPanel

## Contexto

- No login por token, o estado de envio compartilhado troca indevidamente o texto do botão de usuário e senha para “Entrando…”.
- A bandeira pt-BR muda a navegação, mas a visão geral tem textos escritos diretamente em inglês, inclusive detalhes calculados e tempo relativo.

## Arquivos

- `workers/core/cpanel/src/components/login.tsx` e `login.test.tsx`
- `workers/core/cpanel/src/components/overview.tsx` e um teste de comportamento da tela
- `workers/core/cpanel/src/lib/overview.ts` e `i18n.tsx`
- `planning/edger/scripts/cpanel-ui-gate.sh` (o gate ainda exige frases inglesas no componente)

## Detalhes

1. Identificar qual formulário iniciou o login. Durante o envio, bloquear as duas ações; exibir “Entrando…” apenas no formulário ativo. Restaurar os botões após falha.
2. Usar o catálogo existente para todos os textos próprios da visão geral, em pt-BR, en-US e es-ES. Manter identificadores do runtime, códigos de evento, nomes de worker, métricas e papéis recebidos da API como dados, sem traduzi-los arbitrariamente. A bandeira representa a preferência da interface, não dados externos.
3. Traduzir também os detalhes da lista de atenção e o tempo relativo. Preservar a semântica de `Unobserved`: ausência de observação, não falha.

## Tarefas

- [ ] Corrigir o formulário e provar o estado de ambos os botões nos dois caminhos.
- [ ] Traduzir a visão geral e provar troca de idioma em conteúdo visível, atenção e tempo relativo.
- [ ] Revisar por modelo de outra família; executar testes, typecheck, lint, build, gate Rust e validação visual.
- [ ] Integrar via PR e atualizar o lab-dev após os gates.

## Verificação

- Testes de componente com pedidos pendentes controlados e assertivas de texto e `disabled`.
- `bun test` e `bun run --filter @edger/cpanel build`.
- `planning/edger/scripts/cpanel-ui-gate.sh`, com assertivas atualizadas para as chaves de tradução.
- `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check`.
- Revisão independente e inspeção da visão geral em pt-BR no lab-dev.
