# Extensões edger (edger-ext-*)

**Status:** ativo (Epic 06)  
**Origin:** `planning/edger/epics/06-extensibilidade/00-overview.md`

## Princípio choose ONE

Cada crate `edger-ext-*` escolhe **um** modo por crate:

- **Middleware** (hooks `on_request` / `on_response`), ou
- **AuthProvider** / provider especializado (ex: auth), ou
- **WorkerHandler** (dispatch serverless dedicado)

**Anti-padrão (proibido):** `edger-ext-foo` que implementa `AuthProvider` **e** `Middleware` de gateway na mesma crate sem features mutuamente exclusivas no `Cargo.toml`.

## Padrão de registro (story 06.01 — decisão)

| Opção | Status |
|---|---|
| `inventory` | adiado — manutenção incerta |
| `linkme` | adiado — quirks de toolchain |
| **Lista explícita no bin** | **escolhido para v1** |

### Wiring

1. Crate `edger-ext-*` depende **apenas** de `edger-core` (traits).
2. Exporta `pub fn middleware() -> Arc<dyn Middleware>` (ou provider equivalente).
3. Bin `edger` chama `collect_extensions(vec![...])` e passa ao `OrchestratorState`.
4. `ExtensionRegistry` ordena por `priority()` (menor = mais cedo em `on_request`).

Migrar para `inventory`/`linkme` quando houver 3+ extensões estáveis (story futura).

## Checklist nova extensão

- [ ] Crate `edger-ext-<nome>` depende apenas de `edger-core`
- [ ] Implementa **um** trait documentado (choose ONE)
- [ ] Registro explícito no bin `edger` via `collect_extensions`
- [ ] `cargo test -p edger-ext-<nome>` verde
- [ ] Sem dependência de `edger-orchestrator`
- [ ] Não publicar em crates.io manualmente (workspace interno)

## Walkthrough edger-ext-auth (story 06.02)

_Preencher após implementação._

## Template edger-ext-gateway (story 06.03)

_Preencher após implementação._