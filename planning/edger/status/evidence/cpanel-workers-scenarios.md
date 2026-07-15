# Evidência — inventário versionado do cPanel

Data: 2026-07-11

## Cenário vivo

Comando:

```sh
planning/edger/scripts/cpanel-scenario.sh setup
```

O harness foi executado repetidamente sem duplicar versões. Ele mantém o app
`cpanel-scenario` com:

- `1.2.0`: latest/Serving, dois processos, tráfego e p95 reais;
- `1.1.0`: Enabled, URL versionada e volume de tráfego distinto;
- `1.0.0`: Disabled, URL versionada riscada e sem tráfego;
- erro real de dispatch em `/cpanel-scenario/fail`;
- burst concorrente que produziu `WORKER_QUEUE_FULL` e
  `WORKER_QUEUE_TIMEOUT` reais;
- janela real observada no Browser com `req/min`, `p95`, total, processos,
  rejeições e timeouts.

O cleanup é limitado aos três diretórios identificados do cenário:

```sh
planning/edger/scripts/cpanel-scenario.sh cleanup
```

## Estados adicionais

O inventário vivo já contém FetchHandler, RoutesTable, StaticSpa, Fullstack e
WasmModule, workers com uma e múltiplas versões, workers cold/idle, além de URLs
latest e explícitas. A busca foi validada reduzindo a lista a um card; o filtro
Attention reduziu a lista ao app com estado de atenção.

Estados destrutivos/transitórios que não devem permanecer ativos no cPanel são
provados pela suíte isolada do runtime: recycle por TTL/max-requests/erro/OOM,
processo Active/Terminating, cancelamento, queue full e queue timeout.

## Gates

- `planning/edger/scripts/cpanel-ui-gate.sh`: verde.
- `cargo test --workspace`: verde.
- `cargo clippy --workspace -- -D warnings`: verde.
- `cargo fmt --all -- --check`: verde.
- Browser local em `http://127.0.0.1:19080/cpanel/`: validado.

## Observação

`req/min` é calculado no cliente sobre uma janela móvel de até 60 segundos a
partir do contador monotônico real por worker/versão. O p95 é calculado no
runtime sobre uma janela limitada de 64 durações reais por grupo
`namespace + name + version`.

## Proteção do control plane

O worker `cpanel` é identificado como control plane. A última versão habilitada
não pode ser desabilitada:

- a API retorna `409 CPANEL_DEFAULT_REQUIRED`;
- a versão permanece `loaded` e continua resolvendo `/cpanel`;
- a UI mostra o badge `Control plane` e substitui a ação destrutiva por
  `Default version required`, desabilitada;
- quando duas versões do cPanel estão habilitadas, uma delas ainda pode ser
  desabilitada e a maior versão habilitada restante torna-se a default.

## Toolbar de filtros

A toolbar segue o padrão do data-table do APIGate: busca textual, filtros
facetados compactos, seleção visível no trigger, contador de resultados e ação
`Clear` somente quando há filtro ativo. Tipo e Status usam o Select composto do
shadcn; ambos aceitam múltiplas seleções com união dentro da mesma faceta, e o
X do trigger limpa somente aquela faceta. A ordenação deixou de usar `<select>`
nativo e usa o mesmo padrão.

Validação Browser:

- Tipo `Static SPA` reduziu 24 apps para 1;
- Tipo `FetchHandler + Static SPA` exibiu 20 dos 24 apps;
- o X de Type restaurou os 24 apps sem alterar Status ou busca;
- `Clear filters` restaurou os 24 apps;
- ordenação `Name A–Z` foi aplicada pelo Select estilizado;
- o resumo de Apps/Versions/Serving/Attention aparece em card destacado.

## Paginação e expansão

O inventário pagina depois de busca, filtros e ordenação, com 15, 30 ou 60
apps por página, primeiro/anterior/próximo/último e status `Showing X–Y of N`.
Alterar filtro, ordenação ou page size retorna à primeira página. Os detalhes de
versões não criam mais scroll horizontal interno nem `min-width`; o conteúdo
expande no fluxo normal da página. O cabeçalho usa cursor normal e apenas o
chevron mantém cursor acionável.

O cabeçalho de cada app exibe somente o badge com a versão default; as linhas
não repetem mais o badge `latest`. Os Selects compostos usam largura intrínseca
ao texto: Type, Status, ordenação e page size não compartilham `min-width`
artificial. Selects e menus de ação fecham ao clicar fora.

## Processos e tráfego por versão

A barra de capacidade não é decorativa: consome diretamente os campos reais
`activeProcesses`, `idleProcesses`, `terminatingProcesses` e `maxProcesses`
expostos por `/metrics/stats`. Cada segmento representa um slot do pool: verde
ativo, roxo idle, âmbar terminating e cinza disponível. O texto adjacente mostra
processos existentes sobre a capacidade máxima.

Validação Browser com tráfego real no cenário:

- `1.2.0`: `2/2 idle`, `19.0 req/min`, p95 `457ms`, 38 requests totais,
  rejeições e timeouts reais;
- `1.1.0`: `1/2 idle`, `8.0 req/min`, p95 `42ms`, 16 requests totais;
- `1.0.0`: cold e sem tráfego.

A coluna Version usa 90px; Pathname tem largura menor e Processes recebeu espaço
para a barra, a contagem e sinais de fila. Os links de todas as versões usam a mesma cor; a
versão default permanece indicada apenas no header. O elemento clicável do URL
é `inline-block`, limitado à largura do próprio texto, em vez de ocupar a célula.

No Overview, o card superior redundante `Needs attention` foi substituído por
`Total requests`; a lista detalhada de atenção permanece como a fonte acionável
para versões desabilitadas e erros recentes.
