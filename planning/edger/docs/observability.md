# Observabilidade do EdgeR

**Status:** observabilidade local do cPanel, séries curtas, health passivo, probe opcional, console seguro, release/drain, explorador global/scoped, live tail e exportação OTLP opt-in implementados.

## Objetivo

Definir uma fronteira local-first para métricas, eventos, erros, console logs e exportação OTEL. Logs no cPanel e as Admin APIs são capacidade essencial e funcionam sem backend externo; OTLP é um sink opcional e nunca a fonte de leitura do cPanel.

## Hierarquia do produto

1. **Essencial:** captura bounded, retenção curta, consulta/paginação, filtros, correlação, live tail e detalhes no próprio cPanel.
2. **Complementar:** `/metrics` para integração Prometheus e OTLP para Collector/backends externos.
3. **Regra de disponibilidade:** remover ou indisponibilizar toda integração externa não reduz a capacidade local de diagnosticar workers no cPanel.

## Sinais atuais verificados

| Sinal | Superfície atual | Limite atual |
|---|---|---|
| Métricas | `/metrics` e `/metrics/stats` | Snapshot/agregados em memória |
| Erros recentes | Admin API de errors/error-summary | Ring de 20 por nome de worker |
| Dispatch | store operacional e `tracing` estruturado | Até 2.000 eventos globais / 200 por identidade |
| Console JS | stdout/stderr drenados para o store operacional | Bounded por linha, taxa, canal, identidade e capacidade global |
| Explorador local | `/cpanel/observability/logs` e workspace por versão | filtros/deep links, detalhe e páginas de 100 eventos |
| Live tail | SSE root-only | cursor, gap explícito, broadcast 256 e UI pausada por padrão |
| Séries curtas | `/api/admin/observability/series` | 30 s–15 min, buckets de 5–60 s, reset/eviction explícitos |
| Health check | manifesto `healthCheck`, Admin API e cPanel | manual ou on-deploy; sem polling |
| OTEL | traces e logs OTLP gRPC ou HTTP/protobuf | feature/config opt-in; fila e timeout bounded |

## Contratos implementados

### Identidade

- Métricas: `namespace`, `worker`, `version` e enumerações limitadas como `state`/`cause`.
- Eventos e logs: identidade de métricas mais `processId`, `requestId` e `traceId` como campos, nunca como labels.
- A versão default e sua versão explícita compartilham o artefato, mas a UI preserva o pathname/roteamento observado.

### Envelope operacional

Campos allowlisted: ID monotônico, timestamp, source, kind, level, identidade, outcome, status, duration, request/trace ID e mensagem sanitizada opcional. Headers arbitrários, bodies, env, secrets, cookies e paths de filesystem são proibidos.

### Limites

- Store, captura de console, SSE e exporter usam filas bounded.
- Capacidade global, capacidade por identidade, TTL, truncamento e dropped/evicted counters são configuráveis e testados.
- O store atual mantém até 2.000 eventos globais e 200 por identidade no processo atual. Reiniciar o EdgeR limpa a retenção.

### Console dos workers

- stdout/stderr são drenados continuamente por tasks separadas.
- Cada linha aceita até 4 KiB; cada stream aceita até 100 linhas por segundo; a fila entre isolamento e orquestrador possui 1.024 entradas.
- Enqueue usa `try_send`: cliente lento ou fila cheia nunca aplica backpressure ao subprocesso. Drops são somados e associados ao próximo registro aceito.
- ANSI, controles, UTF-8 inválido, tokens e caminhos locais conhecidos são removidos ou redigidos antes de entrar no store.
- Cada registro carrega namespace, worker, versão, process ID e stream. Recycle aloca novo process ID.
- A captura é habilitada por padrão e pode ser desativada com `EDGER_CONSOLE_LOGS_ENABLED=false`; desativada, mantém stdout descartado e apenas o tail interno de stderr para diagnóstico de falha.

### Release e lifecycle

- Release configurado produz `release.started` e termina em `release.succeeded`, `release.failed` ou `release.skipped`; comando, env e stderr não entram no envelope.
- Drain produz `process.drain.started`, `process.drain.completed` ou `process.drain.timed_out`, seguido de `process.terminated`.
- O ACK de shutdown diferencia promises registradas de timeout real. Contagem, duração, causa e process ID ficam disponíveis quando conhecidos.
- TTL, max requests, ephemeral, erro crítico, recycle de stream e shutdown global usam o mesmo produtor bounded.
- ANSI/controle, bytes inválidos e padrões sensíveis passam por sanitização/truncamento central.

### Consulta e live tail

- A API paginada e o SSE aceitam os mesmos filtros allowlisted por worker, versão, processo, source, level, outcome, status, request ID e trace ID.
- A rota global é `/cpanel/observability/logs`; rotas por versão permanecem em `/cpanel/workers/{worker}/{version}/logs`.
- Filtros e o evento selecionado são serializados na URL. Refresh, back/forward e compartilhamento preservam o contexto.
- O SSE exige `x-api-key`; por isso o cliente usa `fetch` streaming em vez de `EventSource`. O store é a autoridade e o broadcast bounded serve apenas para wake-up.
- O cliente começa pausado, deduplica por ID, mantém no máximo 200 linhas vivas e declara cursor expirado como gap, sem replay ilimitado.

### Séries e telas do cPanel

- A rota global `/cpanel/observability` combina snapshots de `/metrics/stats` com séries do store local; `/cpanel/observability/logs` continua sendo o explorador global.
- O workspace versionado usa `/cpanel/workers/{worker}/{version}/observability`, `/files` e `/logs`; refresh, back/forward e deep links preservam a seção.
- A série é calculada no servidor somente sobre eventos `dispatch`, filtrada por `namespace`, `worker` e `version`, com `requestCount`, `errorCount` e p95 por bucket.
- Janelas são limitadas entre 30 segundos e 15 minutos; buckets entre 5 e 60 segundos. `partialWindow=true` informa restart recente ou eviction dentro da janela.
- Request rate no cPanel usa janela móvel de 60 segundos; charts mostram buckets observados. O último snapshot confiável permanece visível com estado `stale` se uma atualização falhar.
- A lista de atenção ordena erros, health passivo, timeout/rejeição e latência. O popover limita altura, fecha por clique externo/Escape e navega para sinais/logs relevantes.

### Health passivo e probe opcional

- Routing (`Default`, `Enabled`, `Disabled`), processo (`Cold`, `Idle`, `Active`, `Terminating`) e confiabilidade recente (`Unobserved`, `Healthy`, `Degraded`, `Failing`) são dimensões distintas.
- Health passivo usa apenas outcomes de tráfego real numa janela de cinco minutos. O header sintético `x-edger-health-check` exclui probes dos contadores e das amostras passivas.
- Manifestos podem declarar `healthCheck.path`, `method` (`GET`/`HEAD`), `mode` (`manual`/`on-deploy`) e `timeout` entre 100 ms e 10 s.
- O endpoint root-only manual emite evento `health_check` sanitizado e não altera routing. O modo on-deploy mantém o candidato fora do roteamento até sucesso; falha remove índice e pacote do disco para não reaparecer após restart/rescan.
- Não existe scheduler de probe: worker sem `healthCheck` nunca recebe request artificial e checks explícitos não funcionam como keep-alive.

### OTEL

- OTLP é compilado pela feature Cargo `otel` e ativado por `EDGER_OTEL_ENABLED=true` mais `OTEL_EXPORTER_OTLP_ENDPOINT`.
- `OTEL_EXPORTER_OTLP_PROTOCOL` aceita `grpc` e `http/protobuf`; traces usam `/v1/traces` e logs/eventos `/v1/logs` no transporte HTTP.
- `OTEL_TRACES_SAMPLER` aceita `always_on`, `always_off`, `traceidratio` e variantes `parentbased_*`; razão inválida fora de 0–1 falha cedo.
- Traces e logs/eventos sanitizados formam o primeiro corte; Prometheus e `/metrics/stats` continuam as superfícies primárias de métricas.
- O propagador W3C Trace Context extrai `traceparent` válido no ingresso, preserva parent/child no dispatch e mantém request ID independente.
- Exporters usam processors assíncronos bounded. Collector indisponível não substitui nem bloqueia o store local; falha é observável no flush e `EDGER_OTEL_REQUIRED=true` torna configuração/init inválida fatal.
- Resource mínimo: `service.name=edger` e `service.version`. Headers arbitrários, bodies, secrets e paths locais não entram no payload exportado.

### Helm e Rancher

- O chart em `charts/edger` mantém `otel.enabled=false` por default e exige endpoint somente quando habilitado.
- `questions.yaml` expõe protocolo, sampler, razão, required e referência a Secret existente para `OTEL_EXPORTER_OTLP_HEADERS`; nenhum token literal entra no ConfigMap.
- Rollback operacional é definir `otel.enabled=false`; isso não remove logs, séries, health ou live tail do cPanel.
- O cPanel não apresenta uma aba `Settings` fictícia nem declara exporter `healthy` sem telemetria real do SDK. Saúde detalhada do exporter/Collector fica no stack externo até existir contrato local verificável de success/drop.

## Ownership

- Epic 21.03: envelope e store.
- Epic 21.04: séries curtas e detalhe.
- Epic 21.05/21.06: consulta e live tail.
- Epic 21.07: console seguro.
- Epic 21.08: OTLP e contexto distribuído.
- Epic 21.09: Helm/Rancher e matriz off/on/config inválida; dashboard de saúde do Collector continua fora do produto essencial.

## Verification contract

- Testes negativos de secrets/headers/bodies.
- Testes de flood, queue cheia, cliente lento, reconnect e shutdown.
- Builds com feature OTEL desligada e ligada.
- Receiver local comprova correlação entre trace e logs/eventos.
- Métricas validam ausência de labels de alta cardinalidade.
- Jornada Browser comprova `View logs`, filtros persistidos no URL, refresh e consulta sem Collector/OTLP.
