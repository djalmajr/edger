# Story 19.C: eszip bundler real

**Origin:** planning/edger/epics/19-runtime-completude/00-overview.md

## Context

- **Problema:** `StubBundler` em `edger-isolation/src/deno/bundle.rs` não empacota dependências reais.
- **Objetivo:** substituir o stub por bundling real para workers multi-file.
- **Valor:** reduz cold-start e remove uma capacidade apenas aparente do runtime.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/deno/bundle.rs` | edit | Trocar `StubBundler` por implementação real de bundle |
| `edger-isolation/src/deno/` | inspect/edit | Integrar artefato gerado ao caminho Deno existente |
| `edger-isolation/tests/` | edit | Cobrir worker multi-file com dependência local |

## Detail

### Critérios de aceite
- [ ] `StubBundler` não é mais a implementação usada em produção.
- [ ] Worker multi-file com dependência local gera artefato executável.
- [ ] Falhas de bundling retornam erro claro, sem mascarar como sucesso.
- [ ] O runtime existente de Deno continua funcionando para worker single-file.

## Tasks

- [ ] Identificar o contrato atual de `bundle.rs`.
- [ ] Implementar bundling real com a menor superfície necessária.
- [ ] Integrar o artefato ao fluxo Deno já existente.
- [ ] Adicionar teste para worker multi-file.

## Verification

```bash
cargo test -p edger-isolation
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
