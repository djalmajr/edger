# Story 21.09: Operação e deploy da integração OTEL

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** exporter sem configuração de deploy, health e prova contra Collector real é difícil de operar e fácil de configurar incorretamente.
- **Objective:** expor configuração segura no chart/Rancher, validar com Collector externo e documentar operação/degradação.
- **Value:** operador habilita OTEL sem reconstruir imagens e entende rapidamente export failures/drops.
- **Constraints:** Collector não é embutido por padrão; secrets usam references; configuração desligada não altera manifests atuais.

## Traceability

- **Prototype:** `OBS-05 OTEL settings/status` no Paper.
- **Business rules:** cPanel é read-only para configuração sensível; chart valida combinações; endpoint mostrado na UI é sanitizado.

## Files

| Path | Action | Reason |
|---|---|---|
| `charts/edger/values.yaml` | edit | Valores OTEL opt-in |
| `charts/edger/questions.yaml` | edit | Perguntas Rancher e validação |
| `charts/edger/templates/` | edit | Env e secret refs |
| `planning/edger/docs/observability.md` | edit | Configuração e troubleshooting |
| `edger-orchestrator/src/tracing_init.rs` | edit | Receiver local e falha de Collector |
| `planning/edger/status/evidence/` | create | Payloads, health, Browser e charts renderizados |

## Detail

### Configuration contract

- `otel.enabled`, `otel.required`, protocol, endpoint, sampler e sampling ratio.
- Headers/tokens vêm de Secret existente por nome/chave; nunca de valor literal em `questions.yaml`.
- Defaults mantêm OTEL desligado e Prometheus ativo.

### Operational proof

- Collector local recebe traces/logs, exporta para sinks de teste e permite verificar correlação.
- Cenários: happy path, endpoint inválido, DNS recusado, receiver lento, queue cheia, restart e shutdown.
- O cPanel continua local-first e não inventa health do exporter. Status/drops do Collector permanecem no stack externo até existir contrato verificável do SDK.

### Scope

- **In:** Helm/Rancher, receiver local, falha de Collector, docs e evidence.
- **Out:** administrar Collector pelo cPanel, instalar backend de vendor e armazenar credenciais no frontend.

### Acceptance criteria

- [x] `helm template` funciona com OTEL off e on, usando apenas secret references.
- [x] `questions.yaml` cobre opções suportadas e endpoint ausente falha no template.
- [x] Prova local mostra trace e logs/eventos sanitizados no receiver.
- [x] Collector indisponível não remove o evento do store local nem interrompe requests.
- [x] Nenhum header sensível é exposto na UI, ConfigMap ou payload OTLP.
- [x] Runbook documenta sampling, queue, shutdown e rollback para OTEL off.

### Dependencies

- 21.08.

## Tasks

- [x] Definir values/questions e secret-ref contract.
- [x] Usar receiver HTTP local descartável no teste de integração, sem backend obrigatório.
- [x] Manter status sensível fora do cPanel enquanto não houver counters confiáveis do SDK.
- [x] Executar matriz off/on/missing endpoint e Collector indisponível.
- [x] Documentar integração genérica sem vendor lock-in.

## Verification

```bash
helm lint charts/edger
helm template edger charts/edger > /tmp/edger-otel-off.yaml
helm template edger charts/edger --set otel.enabled=true --set otel.endpoint=http://collector:4318 > /tmp/edger-otel-on.yaml
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check
```

## Status

completed (2026-07-12) — chart/Rancher off/on/invalid, Secret reference e receiver/falha local verificados. Collector e backend continuam componentes externos opcionais.
