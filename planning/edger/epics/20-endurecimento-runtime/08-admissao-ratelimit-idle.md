# Story 20.08: Admissão com rate-limit e idle timeout

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** o runtime precisa limitar entrada por worker e encerrar leituras ociosas no harness.
- **Objetivo:** adicionar rate-limit/cota por worker e idle/read-timeout no caminho multiproc.
- **Valor:** evita abuso de admissão e processos presos por I/O sem progresso.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-worker/src/pool.rs` | edit | Aplicar admissão por worker antes da execução |
| `edger-isolation/src/multiproc.rs` | edit | Propagar timeouts e falhas do processo |
| `edger-isolation/src/multiproc_harness.mjs` | edit | Encerrar leitura ociosa sem travar o harness |
| `edger-core/src/config.rs` | inspect/edit | Expor limites de admissão se necessário |
| `edger-worker/tests/` | inspect/edit | Cobrir rate-limit, cota e idle timeout |

## Detail

### Critérios de aceite
- [ ] Rate-limit por worker rejeita excesso de requisições de forma observável.
- [ ] Cota por worker é aplicada sem afetar outros workers.
- [ ] Idle/read-timeout interrompe harness sem hang.
- [ ] Erros de admissão e timeout têm status/causa verificável.

## Tasks

- [ ] Identificar ponto único de admissão no pool.
- [ ] Definir limites mínimos por worker.
- [ ] Aplicar idle/read-timeout no harness multiproc.
- [ ] Adicionar testes de excesso, isolamento e leitura ociosa.

## Verification

```bash
rg "rate|quota|idle|timeout|admission" edger-worker edger-isolation edger-core
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
