# Story 10.03: Manifesto e configuracao de modulo

**Origin:** `planning/edger/epics/10-operacao-extensoes-plugins/00-overview.md`

## Context

Buntime usa manifests para apps e plugins. O edger ja tem manifests de workers e capabilities de extensoes, mas ainda nao tem manifesto operacional de modulo que possa ser consumido por UI, MCP e validação local.

**Depende de:** Story 10.01

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/extensions.rs` | edit | Ajustar tipos puros se for necessario representar manifesto sem I/O |
| `edger-orchestrator/src/extensions.rs` | edit | Persistir e carregar manifesto operacional no composition boundary |
| `edger-orchestrator/tests/registry_providers.rs` | edit | Provar compatibilidade com providers existentes |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar formato e limites de configuracao |

## Detail

### AS-IS

- Workers tem manifest loader.
- Extensoes tem metadata/capabilities em codigo Rust.
- Status operacional pode persistir enable/disable.

### TO-BE

- Manifesto operacional descreve modulo, capabilities, menus, providers, dependencia, configuracao segura e requisitos locais.
- Campos de segredo ficam referenciados por nome de env/secret externo, nunca por valor bruto.
- Backward compatibility preserva registro estatico v1.

### Scope

- **In:** shape de manifesto, redaction, leitura no boundary de orquestracao, testes.
- **Out:** manifesto remoto, assinatura publica, marketplace.

### Critérios de aceite

- [ ] Manifesto operacional consegue representar extensoes existentes sem quebrar registro estatico.
- [ ] Campos sensiveis sao omitidos ou redigidos em API/log.
- [ ] Menus/providers/hooks permanecem tipados e descobriveis.
- [ ] Testes provam compatibilidade com pelo menos gateway e auth/keyval.

## Tasks

- [ ] Definir shape minimal do manifesto operacional.
- [ ] Integrar shape com registry sem I/O no core.
- [ ] Implementar redaction para configuracao.
- [ ] Atualizar docs e testes de compatibilidade.

## Verification

```bash
cargo test -p edger-orchestrator --test registry_providers
cargo test -p edger-orchestrator --test registry_hooks
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

