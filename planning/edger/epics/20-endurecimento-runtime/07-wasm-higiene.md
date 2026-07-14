# Story 20.07: Higiene de Wasm

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** Wasm precisa evitar recompilação por requisição e impor limites claros de CPU, memória e body.
- **Objetivo:** usar cache de `Module`, fuel/epoch, `StoreLimits` e suporte a body maior que 64 KiB.
- **Valor:** torna o caminho Wasm mais previsível, barato e seguro.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/wasm/load.rs` | edit | Cachear módulos compilados quando aplicável |
| `crates/edger-isolation/src/wasm/handler.rs` | edit | Aplicar limites no caminho de execução |
| `crates/edger-isolation/src/wasm/wasi.rs` | inspect/edit | Garantir passagem correta de request/body |
| `crates/edger-isolation/src/wasm/mod.rs` | inspect/edit | Ajustar exports internos se necessário |
| `crates/edger-isolation/tests/wasm_integration.rs` | edit | Cobrir cache, limites e body maior |

## Detail

### Critérios de aceite
- [ ] Módulo Wasm não é recompilado a cada requisição.
- [ ] Execução Wasm respeita limite de CPU por fuel ou epoch.
- [ ] Memória é limitada por `StoreLimits` ou mecanismo equivalente.
- [ ] Body maior que 64 KiB é aceito dentro do limite configurado.

## Tasks

- [ ] Mapear o ciclo atual de load/compile/execute.
- [ ] Introduzir cache de `Module` com chave segura.
- [ ] Aplicar limites de CPU e memória no `Store`.
- [ ] Cobrir body maior que 64 KiB em teste ponta-a-ponta.

## Verification

```bash
rg "Module|StoreLimits|fuel|epoch|wasm" crates/edger-isolation/src/wasm crates/edger-isolation/tests
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
