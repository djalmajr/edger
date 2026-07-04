# Story 20.02: OIDC claims para namespaces

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** um JWT válido não pode virar root por inferência fraca ou claims amplas demais.
- **Objetivo:** mapear claims para namespaces e permitir `is_root` somente com role admin explícito.
- **Valor:** fecha uma falha P0 de autorização mantendo a semântica mínima do EdgeR.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/oidc.rs` | edit | Ajustar validação de claims, namespaces e root |
| `edger-orchestrator/tests/` | inspect/edit | Cobrir tokens válidos sem admin, com admin e com claims inválidas |
| `edger-core/src/manifest.rs` | inspect | Confirmar se há contrato de namespace relevante no manifest |

## Detail

### Critérios de aceite
- [ ] Nenhum JWT válido define `is_root` sem role admin explícito.
- [ ] Namespaces derivados de claims ficam escopados ao token validado.
- [ ] Claims ausentes, malformadas ou inesperadas não ampliam privilégio.
- [ ] Testes cobrem usuário comum, admin explícito e erro de autorização.

## Tasks

- [ ] Levantar o fluxo atual de `oidc.rs`.
- [ ] Restringir a regra de root à role admin explícita.
- [ ] Ligar claims de namespace ao contexto autorizado.
- [ ] Adicionar testes de autorização e regressão.

## Verification

```bash
rg "is_root|namespace|oidc|claims" edger-orchestrator/src edger-orchestrator/tests
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
