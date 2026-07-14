# Story 20.01: Sandbox de rede + cache

**Origin:** planning/edger/epics/20-endurecimento-runtime/00-overview.md

## Context

- **Problema:** o worker precisa de controles explícitos de egress e cache para evitar rede aberta e escrita compartilhada entre tenants.
- **Objetivo:** aplicar allowlist de rede por worker e tornar o `DENO_DIR` isolado ou somente leitura após o warm-up.
- **Valor:** reduz a superfície de escape do sandbox sem reintroduzir opinião pesada no runtime.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/config.rs` | inspect/edit | Expor configuração de rede/cache por worker sem quebrar defaults |
| `crates/edger-core/src/manifest.rs` | inspect/edit | Carregar os campos declarativos necessários do manifest |
| `crates/edger-isolation/src/multiproc.rs` | edit | Aplicar flags e diretórios efetivos ao processo Deno |
| `crates/edger-isolation/src/multiproc_harness.mjs` | inspect/edit | Validar impacto do sandbox no harness persistente |
| `crates/edger-isolation/tests/` | inspect/edit | Cobrir allowlist e isolamento do cache |

## Detail

### Critérios de aceite
- [ ] Egress do worker respeita allowlist por worker.
- [ ] Rede aberta via `--allow-net` só ocorre quando configurada como opt-in.
- [ ] `DENO_DIR` não permite escrita compartilhada cross-tenant após o warm-up.
- [ ] Há cobertura para requisição permitida, requisição bloqueada e cache isolado.

## Tasks

- [ ] Mapear configuração atual de rede e cache.
- [ ] Definir o menor contrato de manifest/config necessário.
- [ ] Aplicar o contrato no processo multiproc.
- [ ] Adicionar testes de comportamento observável.

## Verification

```bash
rg "allow-net|DENO_DIR|allow_net|deno_dir"
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

**pending**
