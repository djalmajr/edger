# Decisões — pins do cPanel (28/09/2026)

## D1. Nome longo de worker
- **Decisão:** truncar apenas a apresentação com reticências e manter o identificador integral acessível.
- **Por quê:** evita crescimento da célula sem alterar o nome usado pelo runtime ou pelo clique.
- **Alternativas:** quebrar o texto aumenta a altura da tabela; renomear altera identidade operacional.
- **Reverter:** baixo, restrito ao componente da visão geral.
- **Onde:** `workers/core/cpanel/src/components/overview.tsx`.
- **Status:** em curso.

## D2. Política desabilitada no lab-dev
- **Decisão:** o cPanel consulta as flags efetivas da instância e desabilita o formulário quando tenant routing e weighted routing estão ambos desligados. O overlay do lab-dev explicita `false` para ambos.
- **Por quê:** o ConfigMap já está com `false false`; o formulário atual sugere uma capacidade ativa que o runtime não aplica.
- **Alternativas:** esconder por hostname é frágil; apenas manter flags desligadas não resolve a expectativa da tela.
- **Reverter:** médio, pois envolve contrato da sessão e UI; a reativação operacional é por values/flags.
- **Onde:** sessão Admin API, `RuntimeData`, painel de política e `values-labdev.yaml`.
- **Status:** em curso.

## D3. Permissões compactas
- **Decisão:** mostrar no máximo duas linhas de badges e um `+N` com acesso à lista completa, calculado pela largura disponível.
- **Por quê:** a tabela permanece compacta em diferentes larguras sem ocultar quais permissões foram concedidas.
- **Alternativas:** corte por quantidade fixa erra em telas estreitas; apenas ocultar overflow não informa a contagem.
- **Reverter:** baixo, restrito à célula da tabela.
- **Onde:** `workers/core/cpanel/src/components/api-keys.tsx`.
- **Status:** em curso.

## D4. Edição de permissões de API key
- **Decisão:** `PATCH /api/admin/keys/{id}` atualiza somente `permissions`, valida catálogo/anti-escalada no servidor e invalida o cache imediatamente; key revogada não é editável.
- **Por quê:** evita recriar credenciais para ajustar acesso e garante que remoções tenham efeito na próxima requisição.
- **Alternativas:** recriar key obriga distribuição de segredo novo; editar também escopos/expiração amplia risco e não foi pedido.
- **Reverter:** médio, pois há contrato HTTP e fluxo no cPanel; os dados existentes não exigem migração.
- **Onde:** `edger-core`, Admin API, store SQLite e cPanel.
- **Status:** em curso.
