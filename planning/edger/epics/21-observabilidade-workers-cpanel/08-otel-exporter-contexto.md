# Story 21.08: Exporter OTEL opt-in e contexto distribuído

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** variáveis OTEL são reconhecidas, mas o build atual não liga exporter; requests, dispatch e subprocessos também não possuem contrato completo de contexto distribuído.
- **Objective:** oferecer OTLP real, opt-in e bounded, exportando traces e logs/eventos e propagando contexto W3C sem alterar o modo padrão.
- **Value:** integra EdgeR a Collector/Tempo/Loki/vendors sem transformar infraestrutura externa em dependência do runtime.
- **Constraints:** feature/config opt-in; exporter assíncrono; nenhuma label de alta cardinalidade; fail-open padrão.

## Traceability

- **Prototype:** `OBS-05 OTEL settings/status` e trace link em `OBS-04`.
- **Business rules:** endpoint/headers não aparecem no cPanel; status mostra apenas destino sanitizado, health, last success/error e dropped count.
- **Supersedes:** cauda OTLP pendente de `planning/edger/epics/20-endurecimento-runtime/09-observabilidade-otlp-evento.md`.

## Files

| Path | Action | Reason |
|---|---|---|
| `Cargo.toml` | edit | Dependências/features OTEL alinhadas |
| `crates/edger-orchestrator/Cargo.toml` | edit | Exporter OTLP opt-in |
| `crates/edger-orchestrator/src/tracing_init.rs` | edit | Resource, sampler, batch exporter e shutdown |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Span de ingress/dispatch e atributos allowlisted |
| `crates/edger-orchestrator/src/wire.rs` | edit | Propagar `traceparent`/`tracestate` ao worker |
| `crates/edger-isolation/src/wire.rs` | inspect/edit | Preservar contexto no frame interno |
| `crates/edger-isolation/src/multiproc_harness.mjs` | inspect/edit | Disponibilizar contexto ao handler sem quebrar Fetch |
| `crates/edger-orchestrator/src/observability.rs` | edit | Exportar eventos/logs pelo sink OTEL |
| `crates/edger-orchestrator/tests/otel.rs` | create | Feature off/on, collector fake, queue e shutdown |
| `planning/edger/docs/observability.md` | edit | Configuração, atributos e troubleshooting |

## Detail

### Signal matrix

| Signal | Local | OTLP v1 | Cardinalidade |
|---|---|---|---|
| Traces | tracing fmt/eventos | sim | request/trace IDs somente como atributos |
| Logs/eventos | store bounded | sim | identidade + campos allowlisted |
| Metrics | Prometheus + stats | avaliar após prova | labels limitadas a worker/version/namespace/state/cause |

### Resource and span contract

- Resource mínimo: `service.name=edger`, `service.version`, `service.instance.id` e ambiente configurado.
- Spans: ingress HTTP, dispatch, queue wait, worker execution e resposta; relações preservam parent/child onde houver contexto válido.
- Atributos do worker: namespace, name, version, execution kind e outcome; path completo, headers e body ficam fora.
- `traceparent` inválido é ignorado e registrado por contador, nunca refletido sem validação.

### Runtime behavior

- Sem feature/endpoint: comportamento atual, sem threads/export queue adicionais.
- Com endpoint: batch exporter bounded, timeout/backoff, flush em shutdown com teto e contadores internos.
- Collector indisponível: requests continuam; exporter degrada e expõe health/drops.
- `required=true`: configuração inválida ou init impossível falha no startup com mensagem acionável.

### Scope

- **In:** OTLP traces e logs/eventos, propagação W3C, sampling, resource attributes, queue/timeout/shutdown e testes com receiver local.
- **Out:** tail sampling, baggage arbitrário, profiling, auto-instrumentation JS e backend de consulta OTEL no cPanel.

### Acceptance criteria

- [x] Build/test sem feature OTEL mantém o caminho local padrão.
- [x] Receiver de teste recebe spans e logs/eventos correlacionados quando habilitado.
- [x] Parent context W3C atravessa ingresso e dispatch; request ID continua independente e o header permanece disponível ao worker.
- [x] Secrets, bodies, headers proibidos e paths locais não aparecem no payload OTLP.
- [x] Collector indisponível usa exporter assíncrono e não substitui o store local.
- [x] Flush/shutdown usa timeout bounded e retorna erro do exporter quando o destino está indisponível.
- [x] Métricas continuam em Prometheus/stats e não recebem IDs como labels.

### Dependencies

- 21.03 para envelope; 21.07 para console logs; evento por execução de 20.09 já entregue.

## Tasks

- [x] Fixar matriz de crates/protocolos e feature flags compatíveis com a toolchain.
- [x] Escrever receiver de teste e casos feature off/on/invalid/unavailable.
- [x] Implementar resource, spans, batch exporter, sampler e shutdown.
- [x] Propagar contexto W3C no ingresso/dispatch e preservar os headers do request no wire do worker.
- [x] Adaptar eventos/logs sanitizados ao OpenTelemetry Logs data model.
- [x] Documentar fila bounded, configuração e cardinalidade.

## Verification

```bash
cargo test -p edger-orchestrator --features otel tracing_init::tests
cargo test --workspace
cargo clippy -p edger-orchestrator --all-targets --features otel -- -D warnings
cargo fmt -- --check
```

## Status

completed (2026-07-12) — traces e logs/eventos OTLP gRPC/HTTP-protobuf, W3C, sampler, redaction, feature off/on e falha de Collector verificados; fonte local permanece canônica.
