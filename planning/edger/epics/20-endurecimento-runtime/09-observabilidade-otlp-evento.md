# Story 20.09: Evento por execução e follow-up OTLP

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** traces e eventos de execução precisam ser exportáveis sem forçar OTLP no caminho padrão.
- **Objetivo:** registrar a entrega do evento por execução e manter a rastreabilidade da cauda OTLP transferida ao Epic 21.
- **Valor:** melhora diagnóstico de produção sem tornar observabilidade uma dependência obrigatória.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/tracing_init.rs` | edit | Inicializar export OTLP quando configurado |
| `edger-worker/src/pool.rs` | inspect/edit | Emitir evento por execução com causa/custo |
| `edger-isolation/src/multiproc.rs` | inspect/edit | Propagar causa de execução quando necessário |
| `Cargo.toml` | inspect/edit | Declarar feature/dependência OTLP se necessário |
| `edger-orchestrator/tests/` | inspect/edit | Cobrir configuração ligada e desligada |

## Detail

### Critérios de aceite
- [x] OTLP exporta traces quando a feature/configuração estiver habilitada. Ownership: `planning/edger/epics/21-observabilidade-workers-cpanel/08-otel-exporter-contexto.md`.
- [x] Com OTLP desligado, o runtime preserva o comportamento atual.
- [x] Cada execução emite evento com causa e custo verificáveis.
- [x] Erros de configuração de OTLP são reportados de forma clara.

## Tasks

- [x] Mapear inicialização atual de tracing.
- [x] Adicionar caminho OTLP opt-in na Story 21.08.
- [x] Definir evento mínimo por execução.
- [x] Cobrir modo habilitado, desligado e configuração inválida.

## Verification

```bash
rg "otlp|tracing|span|event|cause|cost" edger-orchestrator edger-worker edger-isolation Cargo.toml
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed / superseded for ownership** — evento por execução entregue no PR #28; exporter, propagação de contexto e cobertura on/off/inválida concluídos na Story 21.08 para manter um único owner.
