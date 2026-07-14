# Story 19.B: Per-worker body override

**Origin:** planning/edger/epics/19-runtime-completude/00-overview.md

## Context

- **Problema:** `max_body_bytes` existe na configuração, mas o caminho de execução ainda usa o limite global fixo de body.
- **Objetivo:** ligar o limite por worker da configuração ao caminho que lê e valida o body da request.
- **Valor:** permite limites específicos por worker sem mudar o teto global para todos.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/config.rs` | inspect/edit | Confirmar normalização e default de `max_body_bytes` |
| `crates/edger-orchestrator/src/wire.rs` | edit | Propagar o limite efetivo até a camada de execução |
| `crates/edger-worker/src/pool.rs` | edit | Aplicar o limite por worker ao ler o body |
| `crates/edger-worker/tests/` | edit | Cobrir aceite/rejeição com limite customizado |

## Detail

### Critérios de aceite
- [x] Worker sem override mantém o limite global atual.
- [x] Worker com `max_body_bytes` menor rejeita body acima do próprio limite.
- [x] Worker com `max_body_bytes` maior aceita body que excederia o limite default, respeitando o teto configurado.
- [x] A validação usa o limite efetivo no caminho de execução, não apenas no parse da config.

## Tasks

- [x] Mapear onde `max_body_bytes` é normalizado.
- [x] Propagar o limite efetivo pelo wire até o pool.
- [x] Substituir uso direto do limite global no execute path.
- [x] Adicionar testes para limite default e limite por worker.

## Verification

```bash
cargo test -p edger-core config
cargo test -p edger-worker
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed**
