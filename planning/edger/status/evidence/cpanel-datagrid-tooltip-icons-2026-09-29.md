# cPanel — DataGrid, ajuda e ações compactas

Estado: concluído localmente em 2026-09-29. Worktree `fix/cpanel-act-warnings`, base `17ec351`. Alterações não commitadas. Prévia própria em `http://127.0.0.1:19081/cpanel/`, token sintético local `test-root`, banco scratch. Sem publicação ou deploy desta rodada.

## Pedidos atendidos

- DataGrid em Usuários: capture `4ad01c67-9421-4313-9131-af916dfab664`, pin `b5e97f9b-c400-4166-a7bc-16ebe4e4d95f`. Reutiliza componente compartilhado, ordenação, páginas 15/30/60, layout fixo, valores longos truncados com valor completo em `title`, permissões em duas linhas. Mantém root-only, root imutável e os handlers existentes.
- Chaves: capture `db2a65ac-a32e-430c-8e32-92f6ace3189a`, pin `b6c87a9f-b3db-4b59-b74b-150223ff0a93`. A rc.6 já usava DataGrid; a captura19080 vinha da rc.4. A prévia atualizada resolve a divergência sem recriar a tabela.
- Ajuda de Chaves: capture `f23f95e6-6357-4f95-845f-4eeaf78910b3`, pin `f8c55d5f-a059-4940-81ce-95062f1f59d1`. O parágrafo `keys.lead` saiu do corpo e aparece em tooltip do botão question junto ao título, conforme WikiHelp do ai-memory-ui. Conteúdo e nome traduzidos pt/en/es, hover/foco/Escape, `role="tooltip"`, id ligado por `aria-describedby`. Usa primitive compartilhada com provider global.
- Ícones de Usuários: capture `b4a4cc11-37f5-44f5-8f15-f432426ddb47`, pin `42e31af8-ed54-4cd2-8adf-4e27717c51ac`. Editar, ativar/desativar, redefinir senha e excluir são ícones na mesma linha, com nomes acessíveis e tooltips traduzidos. Coluna ações160px. Cabeçalhos curtos da tabela evitam sobreposição, sem alterar os rótulos CSV nos formulários. Soma mínima das colunas1074px, overflow horizontal preservado em telas menores.

## Implementação e revisão

Dois implementadores `pi / applianceai01/qwen3.8-27b / high`, painéis reutilizados. Revisor `Claude / claude-sonnet-5-5 / high`. O orquestrador integrou quatro arquivos da ajuda e ajustou alinhamento do título, associação ARIA e larguras/labels de Usuários após prova visual.

Relatórios em `/var/folders/f2/r857c16x45z6p82wsq_0d_v00000gp/T/herdr-soho/wF/reports/`:

- `cpanel-qwen-20260929T182514.md`: DataGrid e quatro testes novos.
- `cpanel-qwen-20260929T184151.md`: ações por ícones e fixtures dos consumidores.
- `release-qwen-20260929T180505.md`: diagnóstico Chaves/build anterior.
- `release-qwen-20260929T181938.md`: implementação da ajuda.
- `review-cpanel-claude-20260929T183722.md`: ajuda PASS, comportamento executado nos três idiomas; sugestão P3 não bloqueante de teste versionado específico.
- `review-cpanel-claude-20260929T184611.md`: Usuários PASS; P3 comentário obsoleto.
- `review-cpanel-claude-20260929T184929.md`: delta de colunas PASS, zero achados novos; comentário corrigido, revisões anteriores mantidas por hashes.

## Gates executados

| Gate | Resultado / evidência |
|---|---|
| `cargo +1.98.0 test --workspace` | PASS, `/tmp/edger-cpanel-pins-rust-tests-20260929.log` |
| `cargo +1.98.0 clippy --workspace -- -D warnings` | PASS, `/tmp/edger-cpanel-pins-rust-clippy-20260929.log` |
| `cargo +1.98.0 fmt -- --check` | PASS, `/tmp/edger-cpanel-pins-rust-fmt-20260929.log` |
| Bun Usuários isolado, versão final | 21 pass / 0 fail, revisor `/tmp/rev9-users/users.log` |
| Bun workers conjunto, versão final | 161 pass / 0 fail, revisor `/tmp/rev9-users/bun-all.log` |
| Vitest cPanel completo, versão final | 144 pass / 13 arquivos, revisor `/tmp/rev9-users/vitest.log` |
| Typecheck final | PASS, revisor `/tmp/rev9-users/tc.log` |
| Diff whitespace final | PASS, revisor e orquestrador |
| Build final | PASS, `/tmp/edger-cpanel-pins-final-build-20260929.log` |
| Path preflight | PASS,310 referências/0 ausentes, `/tmp/edger-cpanel-pins-path-preflight-20260929.log` |
| Refinement lint | PASS,0 RED/46 WARN existentes, `/tmp/edger-cpanel-pins-refinement-20260929.log` |

Zero avisos act/unhandled na matriz final do revisor. Build preserva avisos Vite existentes de plugin legado e chunk grande; não são prova de regressão. Testes de sockets Rust foram executados fora do sandbox. Nenhum delta Rust/chart/dependências nesta fatia.

Hashes revisados antes da emenda19:05 (prefixos SHA256): Users source `82fa89aaa42aa69a`, testes `6b5d23a0659615c3`, fixture `10f39072abb367cc`; main `f4afe8bef0ee733a`, ApiKeys `092489d38c3e7036`, PageTitleHelp `d8dc8bdc1fd7e8c9`, i18n `7169289f9641a4dd`. A correção de act anterior continua em API Keys test `6194bbe4220130e3`.

## Prova no navegador e HTTP

Navegador real, prévia própria19081, autenticada por token sintético, pt-BR:

- Ajuda: centros Y do h1 e botão21.5px, zero parágrafo duplicado no corpo. Hover abre o texto; Tab foca Ajuda e abre conteúdo com role tooltip/id associado; Escape fecha mantendo foco. O revisor também executou comportamento da primitive real em pt/en/es. O hash da ajuda permaneceu igual no ajuste final de colunas.
- Usuários:16 contas sintéticas apenas na DBscratch.15 linhas/Página1de2; próxima página mostra preview-user-16/Página2de2; controle real selecionando30 mostra16 linhas/Página1de1. Isso cobre a limitação de opções da Combobox no HappyDOM.
- Bundle final: headers Nome de usuário/Status/Permissões/Namespaces/Workers/Criado/Ações; tabela1096px na viewport1361px. Quatro ícones presentes, sem texto visível, na mesma linha; todos os limites direitos estão dentro da viewport (último1321px). Hover Editar abre Editar usuário.
- Asset servido na primeira rodada `index-0sRiqf2n.js`: SHA256 `72fd60e2535bcf9bc5c917f9a8655b1d2597d617385df388b034674cdf666dd7`, byte a byte igual ao `dist/assets` daquela versão da worktree.
- O processo compartilhado19080 de `/private/tmp/edger-validation-wF` foi preservado; metadata e fingerprint confirmaram cPanelrc.4 naquela porta. Não atribuir essa instância ao build final.

Screenshots locais no repo root `output/playwright/`:

- `edger-keys-tooltip-final-20260929.png`: ajuda com ARIA corrigido.
- `edger-users-datagrid-icons-final-20260929.png`: quatro ícones e headers curtos, bundle final.

## Limites

Sem deploy lab-dev, VPS ou GHCR desta rodada. Sem commit/push/PR/tag/merge. O teste versionado específico de PageTitleHelp permanece sugestão P3, não gate obrigatório; comportamento teve prova executada no componente real e navegador. Sem prova de touch ou viewport móvel nesta fatia. As contas de prova são sintéticas e locais; senhas geradas não foram exibidas nem persistidas em relatório.

## Emenda19:05 — topo de Usuários igual API Keys

Pin `a402841f-3a83-4669-af4b-02443e9ed8e8`, capture `e1572821-ec85-44fa-8c71-c6fed2da46e7`, batch `a7065e09-afce-4eae-b445-6a9493c98943`. Concluído localmente, na mesma prévia19081.

- `ConsoleUsers` recebe callback opcional `renderPageAction`, repassado apenas após root gate. O botão Novo usuário vai ao mesmo PageActions usado por Chaves, com fallback para consumidores diretos. Não há botão duplicado nem wrapper vazio. O orquestrador acrescentou PlusIcon decorativo para completar o padrão visual de criação de Chaves.
- `main.tsx` usa PageTitleHelp em Chaves e Usuários, com texto da rota correspondente. O parágrafo `users.lead` saiu do corpo; continua na descrição do diálogo existente. Helper, provider, traduções, DataGrid, fixtures, testes, handlers e backend ficaram intactos.
- Pi/Qwen report `cpanel-qwen-20260929T190740.md`; Sonnet reviews `review-cpanel-claude-20260929T191000.md` e `review-cpanel-claude-20260929T191158.md`, no diretório scratch de reports acima. Ambos PASS,0 achados. Hashes atuais: Users `a1c569a34d48017d`, main `4ebc72f4943a9545`; outros hashes mantidos.
- Revisor executou na versão final: Bun Users21/21, Bun conjunto161/161, Vitest144/13 arquivos, typecheck e whitespace PASS. Logs `/tmp/rev11-users/`. Zeroact/unhandled na matriz. Rust test/clippy/fmt reexecutados PASS nesta emenda, logs `/tmp/edger-users-toolbar-rust-{tests,clippy,fmt}-20260929.log`. Build PASS `/tmp/edger-users-toolbar-build-20260929.log`, com os mesmos avisos Vite existentes.
- Browser: Atualizar e Novo usuário têm Y80px; somente um botão Novo usuário, ícone presente. Ajuda na mesma linha do título, texto correto sem parágrafo no corpo, role tooltip e id associado por aria-describedby. Hover/Tab abrem, Escape fecha mantendo foco. Novo usuário abriu o diálogo e Cancelar fechou sem gravar conta. Chrome registrou apenas mensagem DOM verbose sobre password fora de form no diálogo, cujo código não foi alterado nesta emenda.
- Screenshot final `output/playwright/edger-users-toolbar-help-20260929.png` no repo root. Asset atual `index-C03JQEYy.js`, SHA256 `9f560c6b2c742f49c1cb1abd167da1c01e3a791c09f24e566fdd2bf2371c63d0`, servido byte a byte igual ao dist construído. Sem alteração remota/publicação.

## Emenda 2026-09-30 — fonte das permissões ao criar chave

Pin `f529fd66-e8b5-43fe-877e-39184ca3a5ce`, captura
`3dd3586b-8d3c-41c2-b2e8-ded08e581ab1`: o operador pediu remover mono.
Em `api-keys.tsx:484`, o nome de permission usa agora `span text-xs`,
com a fonte normal herdada. Outros identificadores permanecem intactos.
Implementador pi/Qwen: relatório `cpanel-qwen-20260930T000418.md`.
Revisor Sonnet: `review-cpanel-claude-20260930T000604.md`, PASS sem achados.
Typecheck e whitespace aprovados; build do cPanel e gate Rust completo
(test/clippy/fmt) aprovados. Testes Rust exigiram execução fora do sandbox
por bloqueio EPERM no bind de socket Unix. Logs `/tmp/edger-permission-font-*`.
Bundle local recompilado para a prévia em 19081. Sem nova inspeção visual,
sem commit, publicação ou deploy nesta emenda.

## Emenda 2026-09-30 — mono restante e login da prévia

Pins `b08b9c96-dbc7-4d38-97b8-5d1ede2de0aa` (captura
`b14fa75d-077a-4f6f-8fc9-e9ee8eeb5a24`) e
`3f1132a1-da18-40a4-acb6-3f7dc6f119ed` (captura
`757a621c-333a-46a2-b398-0898b6ee908d`). O brief anterior cobria apenas
criar chave; ampliado para editar chave e seletor compartilhado de usuários.
`edit-key-dialog.tsx:90` e `console-users.tsx:114` usam agora span text-xs.
Relatórios `cpanel-qwen-20260930T002112.md` e
`review-cpanel-claude-20260930T002301.md`: PASS, zero achados. Typecheck,
whitespace, Bun e Vitest (29 testes cada) aprovados. Build aprovado.

Navegador: criação e edição de chave e criação de usuário mostram todos os
11 nomes de permissão como SPAN com computed font-family Noto Sans Variable,
sans-serif. Diálogos abertos e cancelados, nenhum dado salvo.

Login por senha não foi removido. GET /api/admin/login-options retornava
passwordEnabled=false/rootSeeded=false: a prévia 19081 não tinha root
semeado. Feito backup SQLite antes da semente, criado arquivo temporário
modo 0600 e reiniciado somente o servidor 19081 com EDGER_ROOT_PASSWORD_FILE,
mesmo banco, token de teste e worktree. Depois, flags true/true; login
root/senha HTTP 200, logout da sessão de prova 204 e acesso por token 200.
Nenhuma senha ou sessão registrada. Navegador confirmou campos Usuário/Senha
e entrada alternativa por token. Nada mudou no código de autenticação.
Sem alteração no servidor 19080, publicação ou deploy remoto.
Gate Rust completo (test --workspace, clippy --workspace -D warnings,
fmt --check) aprovado após esta emenda; logs /tmp/edger-permissions-all-*.

## Emenda 2026-09-30 — tipografia de nomes e datas de Usuários

Pins `fea9d11e-ee1c-4c9d-853a-f4f3b255e545` (captura
`2853a96e-00a2-495b-bd00-79902164f6d5`),
`6ea1baca-2761-48ef-9185-5968705e7647` (captura
`6f060d83-96ee-461e-b803-73f3010ce38e`) e
`29a30080-d552-4e51-aa61-535b3c69375c` (captura
`7fdf1be3-6062-4c15-b8e7-cf27853765d8`). Nome de usuário na tabela e
edição usa span em vez de code; nome e data usam text-sm. Truncamento,
title, tamanho de colunas e comportamento preservados. Não alterar outros
identificadores ou badges fazia parte do brief.

Browser: nome na tabela, nome no diálogo e data renderizam Noto Sans
Variable, sans-serif, 14 px. Nome mede 105 px em célula de 184 px; data
141 px em célula de 157 px no viewport observado. Diálogo aberto e cancelado,
nenhum usuário alterado. Build aprovado; Rust test/clippy/fmt aprovado,
logs `/tmp/edger-users-font-*`.

Sonnet encontrou regressão real nos seletores dos testes: td code passava
a ler namespaces, não username. Duas assertions falharam em Bun/Vitest.
Report `review-cpanel-claude-20260930T004240.md` registra P1 executado.
Emenda autorizou ajustar somente os dois seletores existentes para [title],
sem remover assertions nem adicionar testes tipográficos.
Correção confirmada por `cpanel-qwen-20260930T004418.md` e revisão
`review-cpanel-claude-20260930T004540.md`: P1 resolvido, PASS zero achados.
Revisor executou Bun Users 21/21 e Vitest pacote completo 144/144, typecheck
e whitespace aprovados. Source hash prefixo `3fe148c65c082b5d`, teste
`a3bd7da941232b26`. Nenhum teste removido, nenhuma mudança de conta,
nenhuma publicação ou deploy nesta emenda.
