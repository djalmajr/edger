# Pins do cPanel em lab-dev

## Contexto

O operador marcou quatro ajustes na versão 0.3.2-rc.5: nome de worker longo na visão geral, seção de política de roteamento no lab-dev, excesso de badges de permissões e ausência de edição de permissões de API keys. Na validação local da candidata rc.6, marcou mais dois pontos na tela de chaves: botões de ação em linhas diferentes e tabela larga sem o DataGrid comum.

## Decisões

1. O nome longo será truncado visualmente com reticências; o texto integral ficará acessível por título/tooltip e para tecnologia assistiva. O identificador armazenado e o clique não mudam.
2. O lab-dev já executa com as duas flags de roteamento em `false`. O cPanel deve consultar o estado efetivo da instância e exibir a seção desabilitada, sem formulário nem chamadas de leitura/gravação de política quando ambas estiverem desligadas. Não depender do nome do host para inferir ambiente.
3. A célula de permissões mostrará no máximo duas linhas de badges e um indicador `+N` para as ocultas, com acesso à lista completa. A contagem respeitará a largura real, inclusive responsividade.
4. A edição altera apenas o conjunto de permissões de uma key existente por `PATCH /api/admin/keys/{id}`. O servidor valida catálogo e anti-escalada contra o principal editor e os escopos já existentes. Key revogada não pode ser editada. Toda mutação invalida o cache de autenticação imediatamente; remoção de permissão tem efeito no próximo pedido. Nome, escopos, expiração e valor secreto não são alterados.
5. A ação de criar chave fica na mesma linha de `Atualizar` no cabeçalho da página, com quebra apenas quando a largura exigir. A lista de chaves usa o DataGrid compartilhado; a célula de permissões tem largura limitada e preserva o indicador `+N` e os nomes acessíveis.
6. A tela de chaves e seus diálogos respeitam o idioma selecionado (pt-BR, en-US, es-ES), assim como a paginação do DataGrid.

## Arquivos e fases

- Nome longo: `workers/core/cpanel/src/components/overview.tsx` e teste DOM.
- Estado das flags: `crates/edger-core/src/admin.rs`, `crates/edger-orchestrator/src/admin_api.rs`, `workers/core/cpanel/src/lib/api.ts`, `workers/core/cpanel/src/components/routing-policy.tsx`, `workers/core/cpanel/src/main.tsx`, testes de API/UI e `charts/edger/values-labdev.yaml`, em fases de no máximo cinco arquivos.
- Permissões compactas: `workers/core/cpanel/src/components/api-keys.tsx` e teste DOM.
- Edição de key: `crates/edger-core/src/admin.rs`, `crates/edger-core/src/api_key_store.rs`, `crates/edger-orchestrator/src/api_keys.rs`, `crates/edger-orchestrator/src/admin_api.rs`, `crates/edger-orchestrator/tests/api_keys_admin.rs`, `workers/core/cpanel/src/lib/api.ts`, `workers/core/cpanel/src/components/api-keys.tsx` e teste DOM, divididos em backend e UI.

## Tarefas

- [x] Nome truncado, com valor completo acessível, testado.
- [ ] Flags efetivas expostas e política visualmente desabilitada em lab-dev, testada.
- [x] Badges limitados a duas linhas e `+N` correto em larguras diferentes, testados.
- [x] `PATCH` seguro e diálogo de edição com persistência, anti-escalada e invalidação de cache, testados.
- [x] Ações de chave alinhadas, tabela no DataGrid e coluna de permissões compacta, conferidos em navegador real.
- [ ] Tela de chaves e paginação traduzidas nos três idiomas, conferidas no navegador.
- [x] Revisão independente, gates e deploy de lab-dev.

## Verificação

- Testes DOM de cPanel e testes Rust de API keys e sessão; mutações de UI devem falhar com regressões observáveis.
- Gate JS, Rust workspace, Helm, planejamento e CI da PR.
- No lab-dev: ConfigMap e sessão com as duas flags `false` confirmados; inspeção visual autenticada da seção, badges e nomes longos e prova remota de edição de key com credencial descartável ainda pendentes.
- Após o deploy rc.6, chart, imagem, flags e rotas HTTPS foram verificados. A prova remota de edição com key descartável não foi executada porque a revisão automática exigiu autorização específica para criar/revogar credencial no lab-dev. A inspeção visual autenticada após o upgrade fica para o operador; veja `planning/edger/status/evidence/release-0.3.2-rc.6-2026-09-28.md`.
