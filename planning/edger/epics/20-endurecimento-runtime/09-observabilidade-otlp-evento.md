# Story 20.09: Observabilidade OTLP e evento por execução

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** traces e eventos de execução precisam ser exportáveis sem forçar OTLP no caminho padrão.
- **Objetivo:** adicionar OTLP real atrás de feature/config e emitir evento por execução com causa e custo.
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
- [ ] OTLP exporta traces quando a feature/configuração estiver habilitada.
- [ ] Com OTLP desligado, o runtime preserva o comportamento atual.
- [ ] Cada execução emite evento com causa e custo verificáveis.
- [ ] Erros de configuração de OTLP são reportados de forma clara.

## Tasks

- [ ] Mapear inicialização atual de tracing.
- [ ] Adicionar caminho OTLP opt-in.
- [ ] Definir evento mínimo por execução.
- [ ] Cobrir modo habilitado, desligado e configuração inválida.

## Verification

```bash
rg "otlp|tracing|span|event|cause|cost" edger-orchestrator edger-worker edger-isolation Cargo.toml
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
