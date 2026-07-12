# Checkpoint 2026-07-11: plano do Epic 21 de observabilidade

## Outcome

- O plano existente de observabilidade do cPanel foi consolidado em nove stories.
- Observabilidade local permanece independente de backend externo.
- Captura segura de stdout/stderr ganhou story própria antes do explorador completo.
- A cauda OTLP da Story 20.09 foi transferida para 21.08, junto com propagação W3C e testes feature on/off.
- Helm/Rancher, Collector externo e prova operacional ficaram em 21.09.
- Logs no próprio cPanel foram definidos como produto essencial; OTEL/OTLP e integrações Prometheus permanecem complementares.

## Verified baseline

- `/metrics` e `/metrics/stats` já existem.
- Eventos estruturados de dispatch e ring de erros recentes já existem, mas não formam um store versionado consultável.
- stdout dos workers persistentes não é capturado; stderr é usado apenas para diagnóstico de falha.
- `tracing_init.rs` reconhece configuração OTEL, mas não liga exporter real.

## Planning artifacts

- `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`
- `planning/edger/epics/21-observabilidade-workers-cpanel/07-captura-segura-console-worker.md`
- `planning/edger/epics/21-observabilidade-workers-cpanel/08-otel-exporter-contexto.md`
- `planning/edger/epics/21-observabilidade-workers-cpanel/09-operacao-deploy-otel.md`

## Decision

O produto local será considerado pronto ao entregar logs consultáveis no cPanel, correlação e live tail bounded sem stack externa. OTEL será opt-in e semi-pronto no sentido de possuir contratos, configuração e fronteiras estáveis, mas sua trilha só será considerada pronta quando houver exporter real testado contra receiver/Collector. Apenas reconhecer env vars e emitir warning não satisfaz o aceite.

## Next gate

1. Aprovar os frames `ATT-01` e `OBS-01..05` no Paper.
2. Executar 21.03 → 21.07 → 21.05 e validar logs no cPanel sem Collector.
3. Fechar 21.04/21.06 para detalhe, séries e live tail local.
4. Só então validar crates/protocolos e iniciar a trilha opcional 21.08/21.09.
