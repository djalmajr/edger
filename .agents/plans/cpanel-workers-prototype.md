# Aproximar inventário de Workers do protótipo

**Origem:** solicitação localizada sobre o cPanel existente

## Contexto
- Problema atual: a tela já agrupa múltiplas versões, porém todos os cards ficam expandidos e faltam o cabeçalho analítico, busca, filtros, ordenação e colunas operacionais presentes no protótipo.
- Objetivo da entrega: aproximar a experiência visual do protótipo Paper sem alterar contratos do runtime ou inventar dados não fornecidos pelas APIs.
- Restrições: preservar deploy, enable/disable, URLs versionadas, erros e File Manager; usar os componentes shadcn locais e cores semânticas.
- Referência: `https://app.paper.design/file/01KWZ6Q4CCXHZWB6JP77BMNJCC/1-0/3B-0`, tela “Apps — inventário versionado”.

## Rastreabilidade
- Tela do protótipo: inventário versionado de Apps, com cards recolhíveis e detalhes por versão.
- Regras: a maior versão habilitada continua servindo `latest`; versões antigas mantêm URL explícita.
- Fontes: `workers/cpanel/index.js`, `/api/admin/workers`, `/metrics/stats`.

## Arquivos

| Arquivo | Ação | Motivo | Confiança |
|---|---|---|---|
| `workers/cpanel/index.js` | Alterar | Estrutura, filtros, agrupamento, colunas, estados e atualização periódica de tráfego | core |
| `workers/cpanel/index.css` | Ler/alterar somente se necessário | Ajustes responsivos que não caibam nas classes existentes | provável |
| `workers/cpanel/manifest.yaml` | Alterar | Publicar nova versão do cPanel para o runtime carregar a entrega | provável |
| `edger-worker/src/metrics.rs` | Ler/alterar | Registrar amostras reais de duração por worker/versão caso o shape atual seja insuficiente | provável |
| `edger-worker/src/pool.rs` | Ler/alterar | Alimentar as métricas de tráfego a partir de dispatches reais | provável |
| `edger-orchestrator/src/metrics.rs` | Alterar | Expor contagem e latência reais por worker/versão em `/metrics/stats` | core |
| `edger-orchestrator/tests/metrics_endpoint.rs` | Alterar | Provar o contrato JSON/Prometheus das métricas reais | core |
| `planning/edger/scripts/cpanel-scenario.sh` | Criar | Orquestrar cenários reais e reproduzíveis para preencher todos os estados da interface | core |
| `planning/edger/status/evidence/cpanel-workers-scenarios.md` | Criar | Registrar comandos, respostas e cobertura visual dos cenários | core |

## Detalhe

### Estado atual (AS-IS)
- Cards sempre abertos, uma tabela simples com versão, URL e status.
- Sem busca, filtros, ordenação ou resumo do inventário.
- Métricas por versão são carregadas, mas não aparecem nas linhas.

### Estado desejado (TO-BE)
- Cabeçalho com totais de versões, serving e atenção.
- Busca, filtros por tipo e ordenação.
- Cards recolhíveis, mantendo abertos por padrão os grupos multi-versão ou com atenção.
- Linhas com versão, URL, status, processos e tráfego real por worker/versão.
- O tráfego será exercitado com requisições HTTP reais contra as URLs latest e versionadas; a interface deverá refletir contagem/taxa e latência observadas, sem fixtures ou números mockados.
- Um harness local reproduzível preparará todo o catálogo de estados que a interface é capaz de representar, permitindo revisar a tela completa sem editar dados pelo DevTools.
- Ações existentes preservadas.

### Escopo
- Inclui: tela Workers, responsividade e validação no Browser.
- Não inclui: telemetria externa, dados fabricados, mudança de roteamento ou deploy remoto. Se `/metrics/stats` não expuser latência suficiente por versão, inclui a extensão mínima desse contrato existente.

### Abordagem
- Derivar totais/filtros dos dados já carregados.
- Derivar `req/min` de deltas de contadores monotônicos entre coletas reais e usar percentis reais fornecidos pelo runtime; não inferir p95 a partir de um único valor.
- Gerar tráfego de validação contra `/boom-ui` e `/boom-ui@1.0.0` (ou worker saudável equivalente) e confirmar que cada versão recebe apenas suas próprias amostras.
- Criar um cenário demonstrador idempotente que use deploys, requests concorrentes, enable/disable e workers de teste reais para preencher a matriz abaixo; o harness deve também oferecer limpeza/reversão do estado que ele próprio criou.
- Reusar `Collapsible`, `Badge`, `Button`, `Input` e menus locais.
- Manter a tabela legível em viewport estreito por overflow horizontal.

### Riscos e dependências
- O cPanel é uma SPA estática servida pelo próprio runtime; a validação exige recarregar/publicar a versão adequada.
- Não rotular métricas ausentes como dados reais.
- Percentis exigem uma janela limitada e thread-safe; a implementação deve manter cardinalidade por `name + version + namespace` e evitar crescimento ilimitado.
- Estados transitórios como `active`, fila e timeout precisam permanecer visíveis tempo suficiente para inspeção; o harness deve usar workers controlados por latência/configuração, não sleeps artificiais dentro da UI.

### Matriz obrigatória de cenários visuais

| Dimensão | Estados a produzir com comportamento real | Evidência esperada na interface |
|---|---|---|
| Quantidade de versões | 1 versão; 2+ versões | card compacto/recolhido; card multi-versão aberto |
| Resolução | maior versão habilitada; versão antiga explícita; fallback após disable da latest | `latest → x.y.z`, URL latest e URLs `@versão` corretas |
| Disponibilidade | Serving; Enabled; Disabled; sem versão habilitada | badge/status e contadores coerentes |
| Saúde/erros | saudável; erro recente de dispatch; unhealthy/recycle por erro | estado de atenção e diálogo de erros com ocorrência real |
| Processos | absent/cold; idle; active; múltiplos processos; terminating/recycled quando observável | contagem e indicador de processos por versão |
| Tráfego | zero amostras; baixo volume; burst concorrente; versões com volumes diferentes | vazio honesto, `req/min` e `p95` distintos e reais |
| Backpressure | fila ocupada; rejeição por limite; timeout de fila | indicadores de queued/rejected/timeout e atenção |
| Recycle | TTL, max-requests, erro e OOM quando executável com segurança local | causa real exposta sem fingir estados indisponíveis |
| Tipos | FetchHandler, RoutesTable, StaticSpa, Fullstack e WasmModule existentes | filtros/totais por tipo e badges corretos |
| Escopo | unscoped e namespaced | nome/namespace, URL e agrupamento corretos |
| Ações | abrir/copiar URL, arquivos, enable/disable, erros e deploy de nova versão | menus e diálogos funcionais em cada estado aplicável |
| Toolbar | busca sem resultado/com resultado, filtros, ordenações e reset | inventário e totais filtrados corretamente |
| Responsividade | desktop do protótipo e viewport estreito | tabela sem perda de ações ou informação crítica |

Estados perigosos ou não determinísticos, especialmente OOM, só entram no harness se puderem ser limitados ao worker de teste e executados sem comprometer o processo do cPanel; caso contrário devem aparecer como cenário automatizado isolado e ser documentados como não mantidos ao vivo.

## Critérios de aceite
- [x] `boom-ui` aparece como um único app com duas versões e `latest → 1.1.0`.
- [x] Cards podem ser recolhidos/expandidos.
- [x] Busca e filtros reduzem o inventário corretamente.
- [x] Processos usam `/metrics/stats`; tráfego não é fabricado.
- [x] Requisições HTTP reais alteram contagem/taxa e latência somente da versão realmente chamada.
- [x] A coluna Traffic mostra `req/min` e `p95` provenientes de amostras reais, com estado vazio honesto antes de haver tráfego.
- [x] A matriz obrigatória foi exercitada; cada estado visível provém de uma ação/runtime real ou está explicitamente documentado como cenário isolado seguro.
- [x] O harness é idempotente, tem cleanup e não deixa processos/versões temporárias sem identificação.
- [x] Deploy e menus por versão continuam disponíveis.
- [x] Tela validada visualmente no Browser.

## Plano test-first
- Comportamento: derivação de grupos, totais, busca/filtro, versão serving, processos, isolamento das métricas reais por versão e cobertura de toda a matriz de estados.
- Primeiro teste: integração do endpoint de métricas enviando tráfego real a duas versões e comprovando contadores/latência separados; depois gate estático/DOM do cPanel para os controles e a estrutura multi-versão.
- Nível preferido: integração Rust/HTTP + teste estático focado + validação E2E no Browser com o runtime real.
- Valor frontend: protege o fluxo crítico de inventário/versionamento e ações operacionais.
- Evitar: snapshots integrais frágeis de classes Tailwind.

## Tarefas
- [x] Mapear imports/componentes e o shape de métricas. **Pronto quando:** nenhum dado visual depende de campo inexistente.
- [x] Criar integração que gera tráfego real por versão e falha enquanto contagem/latência isoladas não estiverem no contrato. **Pronto quando:** Red reproduzível.
- [x] Implementar a instrumentação mínima e o contrato `/metrics/stats`. **Pronto quando:** o teste de tráfego real fica verde sem mocks.
- [x] Criar gate focado que falha para os controles/estrutura novos. **Pronto quando:** Red reproduzível.
- [x] Implementar cabeçalho, toolbar, cards recolhíveis e colunas operacionais. **Pronto quando:** gate focado verde e Traffic usa somente o contrato real.
- [x] Implementar harness de cenários e cleanup. **Pronto quando:** uma execução prepara o catálogo visual completo e uma segunda execução permanece idempotente.
- [x] Preservar ações e ajustar responsividade. **Pronto quando:** deploy, URL e enable/disable continuam presentes.
- [x] Percorrer a matriz no Browser e registrar evidências. **Pronto quando:** todos os estados aplicáveis foram vistos e os estados isolados têm prova automatizada.
- [x] Executar gates do repositório e validação Browser. **Pronto quando:** resultados e limitações estão registrados.

## Verificação
- [x] Gate estático focado do cPanel.
- [x] Integração com tráfego real contra versões distintas e asserções de contagem/latência.
- [x] `planning/edger/scripts/cpanel-scenario.sh setup`, segunda execução idempotente e `cleanup`.
- [x] `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check`
- [x] Validação manual no Browser cobrindo a matriz de cenários, incluindo `boom-ui` em duas versões.
- [x] Critérios de aceite atendidos.

## Próximo passo recomendado
- Após implementar: fechamento via `/agile-status`.
