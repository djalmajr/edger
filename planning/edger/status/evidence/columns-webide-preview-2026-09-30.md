# Visibilidade de colunas e WebIDE — prévia local

## Pedido e escopo

Pins de colunas: 26b70e9f-9bd9-4ac6-8077-ad8e2e3b708f (API Keys,
captura c8d2f1de-a4ff-4baf-97da-d2906fc1ebdc) e
e0799322-d93f-4208-8c3a-cacb8f08581a (Usuários, captura
6a0354ed-c9b4-47bf-84a9-d8f4622cd1a1). Operador corrigiu pedido de remoção
para ocultar por padrão, com escolha de visibilidade como appliance.
Implementer cpanel-qwen, pi applianceai01/qwen3.8-27b high.
Segundo pedido: screenshot WebIDE com SPA_ENTRYPOINT_INVALID no servidor
local 19081. Implementer release-qwen, mesmo kind/model/effort.

## WebIDE — concluído

Manifest usa dist/index.html; dist não existia no worktree servido. Build
bun run build aprovado no WebIDE de cpanel-act-warnings, sem edição de
source/config/deps, apenas artifacts ignorados. Relatório scratch
release-qwen-20260930T070346.md. HTTP /webide antes500/depois200, título
EdgeR WebIDE, JS index-BHsGqwi9.js, CSS index-CvQhgm0E.css e fonte com200.
Browser pelo orquestrador: nova página em contexto local, dashboard renderiza,
HTTP200, sem pageerror durante carregamento inicial. Nenhum projeto criado,
importado ou alterado; isso não é prova E2E dessas ações. Página de prova
fechada depois de ler estado. Sem restart/deploy/publicação.

## Gates do orquestrador

Rust test --workspace, clippy --workspace -- -D warnings, fmt --check,
cargo +1.98.0: PASS. Logs /tmp/edger-columns-{rust-tests,clippy,fmt}-20260930.log.
Nenhuma alteração Rust/chart/dependências nesta fatia.

## cPanel

Implementação entregue nos relatórios cpanel-qwen-20260930T070340.md
(visibilidade) e cpanel-qwen-20260930T072727.md (layout). Menu acessível e
traduzido nas duas telas; preferences separadas em edger.cpanel.columns.keys
/users. permissions false por padrão; ações nunca ocultadas. Helper sanitiza
IDs/booleanos e trata inclusive getter localStorage que lança. Width/colSpan
usam colunas visíveis. 42 testes Bun focados/157 Vitest cPanel aprovados pelo
implementador; typecheck/whitespace aprovados. Revisão independente final aprovada: review-cpanel-claude-20260930T074933.md,
PASS sem achados.

Browser: defaults ocultos nas duas telas, mouse e Space/Escape, show/hide,
recarregamento e independência entre tabelas confirmados. Status também foi
ocultado/recarregado e restaurado. Nenhum dado de chave ou usuário alterado.
Preferências da prova restauradas para permissions false. No primeiro build,
abrir menu causou BaseUI error31: GroupLabel fora de Group; corrigido pelo
implementador e reexecutado com menu funcionando em browser real.

Layout API Keys: DataGrid elasticColumnId opt-in em name. A 1361px o nome
mede323px, keyPrefix/workers140px, status96px, lastUsedAt/expiresAt154px,
actions104px; final1344px dentro do viewport. A999px, min-width928px provoca
rolagem interna, pageWidth999px, ações alcançadas por scroll em x878–982px.
Screenshot /Users/djalmajr/Developer/djalmajr/edger/output/playwright/edger-key-columns-20260930.png. Não reduzir
fontes nem mudar outros defaults foi condição da emenda.

## Novos pins de layout e tooltip

Layout: pin cb72a600-08b0-4011-9d22-379e0f8d45e6, captura
2cf1c758-7c9d-482a-9824-de328c38b504. Tooltip de alerta Workers:
pin fae2c008-1fee-4eb2-aa64-02711363a551, captura
4bab2633-fc77-4e90-9315-1b017bde8366. Badge soma versões desabilitadas e
erros registrados (não é contagem de versões). Tooltip entregue no relatório release-qwen-20260930T072732.md. Helper em
main.tsx usa header button existente como trigger, useId/roletooltip/link
aria-describedby explícitos, sem botão/tabIndex aninhado. Key traduzida3locales
com cada parcela do badge; cálculo mantido. Typecheck, i18n7 testes e
whitespace aprovados pelo implementador. Item browser partial dele foi fechado
pelo orquestrador: hover no grupo webide mostrou "0 versões desabilitadas ·
2 erros registrados" (counts reais da prévia), id/aria-describedby iguais.
Tab do grupo cpanel focou webide e abriu tooltip; Escape fechou mantendo foco.
Click expandiu tabela (0→1) e segundo click recolheu (1→0). Screenshot
/Users/djalmajr/Developer/djalmajr/edger/output/playwright/edger-workers-alert-tooltip-20260930.png. Nenhum worker
habilitado/desabilitado e nenhum projeto alterado durante prova.

## Revisão e correção de fixture

Sonnet report review-cpanel-claude-20260930T073128.md verificou funcionalidades
mas trouxe P2 executado: mocks globais novos do Bun não exportavam RadioGroup,
ActivityIcon/XIcon/SettingsIcon para Login/Overview/RoutingPolicy; Bun conjunto
148pass/3fail/3errors, enquanto Vitest157 e Bun42 focados passavam. O
verdict pass com P2 aberto não é fechamento da fatia. Correção entregue por cpanel-qwen em cpanel-qwen-20260930T073537.md: fixture
Bun compartilhada com os exports usados pelos consumidores, sem reduzir
assertions. Reexecução independente Sonnet em
review-cpanel-claude-20260930T074933.md: 174 testes Bun, zero falhas/erros;
157 testes Vitest em 14 arquivos, typecheck e whitespace aprovados. P2
resolvido; PASS sem achados. Revisão do tooltip em
review-cpanel-claude-20260930T073752.md também PASS sem achados.

## Fechamento

Build cPanel aprovado. Path preflight: 310 referências, zero caminhos ausentes.
Refinement lint: zero RED, 46 WARN existentes, 161 INFO e 4215 OK; gate PASS.
Logs /tmp/edger-columns-{cpanel-build,path-preflight,refinement}-20260930.log.
Relatórios de workers em /var/folders/f2/r857c16x45z6p82wsq_0d_v00000gp/T/herdr-soho/wF/reports/.
Entrega concluída e revisada na prévia local 19081. Sem commit, push,
publicação ou deploy remoto. Fluxos de criação/importação do WebIDE não
foram exercitados nesta fatia.
