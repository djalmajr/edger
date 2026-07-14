# Story 19.C: Bundling condicional via deno bundle

**Origin:** planning/edger/epics/19-runtime-completude/00-overview.md

## Context

- **Problema:** `StubBundler` em `crates/edger-isolation/src/deno/bundle.rs` não empacotava dependências reais.
- **Objetivo:** substituir o stub por bundling condicional via `deno bundle` para workers multi-file, com fallback relativo e import direto para worker single-file.
- **Valor:** reduz cold-start e remove uma capacidade apenas aparente do runtime.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/deno/bundle.rs` | edit | Trocar `StubBundler` por bundling condicional via `deno bundle` |
| `crates/edger-isolation/src/deno/` | inspect/edit | Integrar artefato gerado ao caminho Deno existente |
| `crates/edger-isolation/tests/` | edit | Cobrir worker multi-file com dependência local |

## Detail

### Critérios de aceite
- [x] `StubBundler` não é mais a implementação usada em produção.
- [x] Worker multi-file com dependência local gera artefato executável.
- [x] Falhas de bundling retornam erro claro, sem mascarar como sucesso.
- [x] Worker single-file continua usando import direto, sem bundling completo.

## Tasks

- [x] Identificar o contrato atual de `bundle.rs`.
- [x] Implementar bundling condicional via `deno bundle` com a menor superfície necessária.
- [x] Integrar o artefato e o fallback relativo ao fluxo Deno já existente.
- [x] Adicionar teste para worker multi-file.

## Verification

```bash
cargo test -p edger-isolation
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**completed**
