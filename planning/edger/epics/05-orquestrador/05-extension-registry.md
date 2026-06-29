# Story 05.05: Extension registry (ExtensionRegistry, registro estático, on_request short-circuit)

**Origin:** `planning/edger/epics/05-orquestrador/00-overview.md`

## Context
- **Problema:** Pipeline não executa hooks; extensões não têm ponto de registro nem ordem de execução.
- **Objetivo:** `ExtensionRegistry` no orchestrator com registro estático explícito e cadeia `on_request` com short-circuit.
- **Valor:** Base Open/Closed para `edger-ext-*`; paridade com topological sort + priority do Buntime plugin system.
- **Restrições:** Registro compile-time via lista explícita no bin (inventory/linkme detalhado no Epic 06); sem dlopen.

## Traceability
- **Source docs:** `planning/edger/design.md` (Extension traits, PR 8), `planning/edger/design.md (hooks/extensions; ai-memory zommehq/buntime)`
- **Design PR:** PR 8 — `feat(core + orchestrator): extension traits, registry, static registration + hook execution`
- **Depende de:** Story 05.03, Epic 02 (`Extension`, `Middleware` traits)

## Files

| Path | Ação | Motivo |
|---|---|---|
| `edger-orchestrator/src/registry.rs` | criar | `ExtensionRegistry`, hook runners |
| `edger-orchestrator/src/pipeline.rs` | alterar | substituir hook stub por registry |
| `edger-orchestrator/src/hooks.rs` | criar | `run_on_request`, `run_on_response`, lifecycle |
| `edger-orchestrator/tests/registry_hooks.rs` | criar | priority + short-circuit |
| `edger-orchestrator/src/bin/edger.rs` | alterar | `registry.register(...)` explícito |
| `edger-orchestrator/tests/mock_extension.rs` | criar | Middleware de teste |

## Detail

### AS-IS
`HookRunner` stub no-op; sem `ExtensionRegistry`.

### TO-BE
- `ExtensionRegistry`:
  - `register(Box<dyn Extension>)` ou `register_arc(Arc<dyn Middleware>)`
  - Índice por `name()`; rejeitar duplicatas
  - Ordenação por `priority()` (menor = mais cedo em `on_request`)
  - Lifecycle: `on_init`, `on_server_start`, `on_shutdown`
- `run_on_request`:
  - Itera middlewares em ordem
  - `Some(response)` → short-circuit (não chama pool nem middlewares restantes)
  - `None` → continua (req pode ter sido mutada)
- `run_on_response`: ordem inversa ou mesma (documentar — alinhar Buntime)
- Integração com `build_pipeline(registry, ...)`
- Exemplo no bin: registrar mock extension de teste

### Escopo
- **In:** registry, ordenação, short-circuit, lifecycle hooks, testes
- **Out:** padrão inventory/linkme e docs "choose ONE" (Epic 06.01); crate `edger-ext-auth` real (Epic 06.02)

### Critérios de aceite
- [x] Dois middlewares: primeiro retorna `Some(418)` → pool não chamado
- [x] Prioridade `-10` executa antes de `0`
- [x] `on_init` chamado na subida; `on_shutdown` no ctrl_c
- [x] Extensão duplicada por `name()` → erro no register
- [x] `publicRoutes` bypassa `on_request` (coordenado com 05.04 via `skip_hooks`)

## Pendências
- Padrão inventory/linkme documentado no Epic 06.01.
- `edger-ext-auth` real permanece Epic 06.02.

### Dependências
- Story 05.03 (pipeline), Story 05.04 (bypass público)
- Epic 02: traits Extension/Middleware

## Test-first plan
1. **Red:** registry vazio → `run_on_request` retorna `None`
2. **Red:** middleware retorna `Some(res)` → pipeline retorna res sem fetch
3. **Red:** dois middlewares, ordem por priority verificada via contador
4. **Red:** `on_shutdown` invocado uma vez
5. **Green:** `registry.rs` + `hooks.rs`
6. **Refactor:** extrair `MiddlewareChain` testável

**Nível:** unit + integração (`registry_hooks.rs`)

## Tasks
- [x] Implementar `ExtensionRegistry` com storage `Vec<Arc<dyn Middleware>>` + metadata
- [x] Implementar sort estável por priority
- [x] Implementar `run_on_request` / `run_on_response` (`hooks.rs`)
- [x] Implementar lifecycle runners
- [x] Substituir stub no `pipeline.rs`
- [x] Criar mock `TestMiddleware` em `tests/registry_hooks.rs`
- [x] Registro explícito no bin (placeholder para Epic 06)
- [x] Documentar contrato short-circuit em `hooks.rs`
- [x] Verificar bypass publicRoutes com `skip_hooks`

## Verification
```bash
cargo test -p edger-orchestrator registry
cargo test -p edger-orchestrator
cargo test --workspace
cargo clippy --workspace -- -D warnings
bun test
```