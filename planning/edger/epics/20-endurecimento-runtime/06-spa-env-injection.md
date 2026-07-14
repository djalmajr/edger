# Story 20.06: SPA env injection e base href

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** SPAs precisam receber configuração de runtime sem rebuild e sem quebrar assets servidos sob base path.
- **Objetivo:** injetar `window.__env__` em runtime e reescrever `base href` quando configurado.
- **Valor:** melhora operação de apps fullstack/SPA preservando o runtime minimalista.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/static_spa.rs` | edit | Servir HTML com env de runtime e base ajustada |
| `crates/edger-isolation/src/fullstack.rs` | inspect/edit | Garantir compatibilidade com adapters fullstack |
| `crates/edger-core/src/manifest.rs` | inspect/edit | Expor contrato declarativo de env/base se necessário |
| `crates/edger-isolation/tests/` | inspect/edit | Cobrir HTML, env e assets sob base path |

## Detail

### Critérios de aceite
- [ ] HTML da SPA expõe `window.__env__` com valores de runtime.
- [ ] Alteração de env não exige rebuild do bundle estático.
- [ ] `base href` é reescrito quando configurado.
- [ ] Assets continuam resolvendo corretamente sob o base path.

## Tasks

- [ ] Mapear o caminho atual de `static_spa.rs`.
- [ ] Definir origem permitida dos valores de env.
- [ ] Inserir env e base no HTML servido.
- [ ] Adicionar testes de HTML e resolução de assets.

## Verification

```bash
rg "__env__|base href|static_spa|fullstack" edger-isolation edger-core
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
