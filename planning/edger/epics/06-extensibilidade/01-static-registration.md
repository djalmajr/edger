# Story 06.01: Registro estático de extensões (inventory/linkme, regra choose-ONE)

**Origin:** `planning/edger/epics/06-extensibilidade/00-overview.md`

## Context
- **Problema:** Extensões precisam ser descobertas em compile-time, mas o padrão (inventory vs linkme vs lista explícita) não está decidido nem documentado.
- **Objetivo:** Escolher e implementar UM mecanismo de registro estático + documentar regra "choose ONE" por crate.
- **Valor:** Autores de `edger-ext-*` sabem como registrar sem editar orchestrator core; OCP real.
- **Restrições:** Sem dlopen; decisão registrada em planning; bin `edger` é ponto de composição final.

## Traceability
- **Source docs:** `planning/edger/design.md` (Key Decision #7, PR 8, Risks — choose ONE API mode)
- **Design PR:** PR 8 (static registration)
- **Depende de:** Epic 05 Story 05.05 (`ExtensionRegistry`)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `planning/edger/docs/extensions.md` | criar | padrão escolhido + choose ONE |
| `crates/edger-orchestrator/src/registry.rs` | alterar | hook para auto-collect ou macro |
| `crates/edger-orchestrator/src/bin/edger.rs` | alterar | wiring final |
| `Cargo.toml` (workspace) | alterar | dep inventory ou linkme |
| `crates/edger-orchestrator/tests/static_registration.rs` | criar | prova collect |
| `AGENTS.md` | alterar | regra extensões |

## Detail

### AS-IS
Registro manual `registry.register(...)` no bin; sem macro collect; sem docs.

### TO-BE
**Decisão (a tomar na implementação — documentar resultado):**

| Opção | Prós | Contras |
|---|---|---|
| `inventory` | Coleta automática via distributed slice | Manutenção da crate |
| `linkme` | Similar, linker-based | Toolchain quirks |
| Lista explícita no bin | Simples, explícito, zero magic | Editar bin ao adicionar ext |

Recomendação de planning: começar com **lista explícita no bin** se inventory/linkme atrasar; migrar para inventory quando segunda ext existir.

- Macro ou fn `collect_extensions() -> Vec<Box<dyn Extension>>` se inventory/linkme
- Doc **choose ONE**:
  - Crate de auth → só `AuthProvider` / `Middleware` de auth
  - Crate gateway → só `Middleware` de roteamento
  - Não misturar `WorkerHandler` + `Middleware` na mesma crate sem `Cargo.toml` features mutuamente exclusivas
- Checklist para nova extensão em `planning/edger/docs/extensions.md`

### Escopo
- **In:** decisão, impl mínima, docs, teste de collect
- **Out:** crates ext reais (06.02–06.03)

### Critérios de aceite
- [x] `planning/edger/docs/extensions.md` existe em português com padrão escolhido
- [x] Teste prova que extensão registrada via padrão aparece no registry
- [x] AGENTS menciona: ext depende só de `edger-core`; nunca publicar manualmente crates.io
- [x] Regra choose ONE com exemplo anti-padrão (crate que faz auth + gateway — proibido)

### Dependências
- Story 05.05

## Test-first plan
1. **Red:** `collect_extensions()` retorna vec vazio sem registradores — falha se fn não existe
2. **Red:** crate test `register_extension!()` adiciona mock à lista
3. **Green:** implementar padrão escolhido
4. **Refactor:** macro `edger_extension!` para reduzir boilerplate

**Nível:** integração (`static_registration.rs`)

## Tasks
- [x] Spike: inventory vs linkme vs explicit — decisão **lista explícita no bin** em `docs/extensions.md`
- [x] Implementar `collect_extensions` + `ExtensionRegistry::from_explicit`
- [x] Escrever `planning/edger/docs/extensions.md` (choose ONE, deps, registro, testes)
- [x] Atualizar `AGENTS.md` com regras de extensão
- [x] Teste mock em `tests/static_registration.rs` (3 cenários)
- [x] Atualizar overview epic 06 com decisão tomada

## Pendências
- Migrar para `inventory`/`linkme` quando 3+ extensões estáveis (não bloqueante v1).

## Verification
```bash
cargo test -p edger-orchestrator static_registration
cargo test --workspace
cargo clippy --workspace -- -D warnings
bun test
```