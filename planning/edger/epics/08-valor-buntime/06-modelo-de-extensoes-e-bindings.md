# Story 08.06: Modelo de extensões, providers e bindings

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** Buntime entrega valor por plugin registry, providers, hooks, menus e websocket handlers. edger já tem extensões Rust, mas precisa evoluir o modelo sem perder isolamento e simplicidade.
- **Objetivo:** Formalizar o contrato de extensões edger para providers/bindings e hooks operacionais usados pelas stories de estado, gateway e operação.
- **Valor:** Novas capacidades entram por extensão sem editar o core e sem criar acoplamento com orchestrator dentro dos crates `edger-ext-*`.
- **Restrições:** Cada crate escolhe um modo principal; extensão não depende de `edger-orchestrator`; registro v1 continua explícito/estático.

## Traceability
- **Source docs:** `planning/edger/docs/extensions.md`, `planning/edger/epics/06-extensibilidade/00-overview.md`, `planning/edger/docs/value-parity-matrix.md`
- **Buntime refs:** plugin-system docs e `apps/runtime/src/plugins/registry.ts`
- **Prototype refs:** none.
- **Business rules:** providers são capacidades declaradas e testáveis, não acesso global implícito.

## Files

| Path | Action | Reason |
|---|---|---|
| `planning/edger/docs/extensions.md` | edit | Registrar contrato provider/binding v1 |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar providers registrados e `EDGER_STATE_DIR` |
| `crates/edger-core/src/extension.rs` | edit | Tipos puros para providers, hooks e capabilities |
| `crates/edger-core/src/bindings.rs` | edit | Alinhar bindings com providers |
| `crates/edger-core/src/lib.rs` | edit | Reexportar vocabulário de extensões |
| `crates/edger-orchestrator/Cargo.toml` | edit | Permitir wiring real de providers no bin |
| `crates/edger-orchestrator/src/bin/edger.rs` | edit | Registrar providers SQL/KV/queue no composition root |
| `crates/edger-orchestrator/src/registry.rs` | edit | Resolver providers registrados |
| `crates/edger-orchestrator/src/service_bindings.rs` | edit | Validar provider antes de injetar binding |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Passar registry para binding lookup |
| `crates/edger-orchestrator/tests/registry_providers.rs` | create | Testes de provider lookup, conflito e dependência |
| `crates/edger-orchestrator/tests/state_services.rs` | edit | Registrar providers no estado de teste |
| `edger-ext-gateway/src/lib.rs` | edit | Declarar capabilities gateway |
| `edger-ext-auth/src/lib.rs` | edit | Declarar capabilities auth |
| `edger-ext-turso/src/lib.rs` | edit | Declarar capability SQL provider |
| `edger-ext-keyval/src/lib.rs` | edit | Declarar capabilities KV/queue e dependência SQL |
| `planning/edger/docs/value-parity-matrix.md` | edit | Evidência de plugin/extension model |

## Detail

### AS-IS
- Extensões existem como crates e registry estático.
- Contratos de provider e binding ainda não são centrais.
- Buntime tem registry com dependencies, providers e hooks de lifecycle, mas isso não deve ser copiado literalmente.

### TO-BE
- `ExtensionCapability` descreve middleware, auth provider, worker handler, service provider, menu contribution e hooks permitidos.
- Registry detecta conflitos e dependências ausentes em startup.
- Provider lookup é explícito por namespace/capability.
- Menus e app catalog podem ser declarados como dados, sem forçar UI final.

### Scope
- **In:** contrato de provider, conflito, dependência, capability declaration, docs e testes.
- **Out:** dynamic loading de crates, hot reload completo, marketplace, websocket plugin completo.

### Story-time plan

**Modo:** plan-then-implement autorizado pelo pedido de continuidade da Epic 8.

**Decisões travadas:**

| Decisão | Escolha | Motivo |
|---|---|---|
| Fonte de verdade de capabilities | `crates/edger-core/src/extension.rs` | Mantém contratos puros e compartilháveis por `edger-ext-*` sem depender do orchestrator |
| Registro de providers | slots explícitos em `ExtensionRegistry` | Segue o padrão v1 de registro estático e detecta conflito cedo |
| Dependências | `ExtensionDependency` por capability requerida | Evita copiar topological loader do Buntime, mas preserva o valor de falhar antes do runtime aceitar configuração inválida |
| Binding lookup | `resolve_service_bindings` consulta registry | Worker só recebe binding se existe provider declarado para o `BindingKind` |
| Menu/catalog | capability tipada e docs, sem UI final | Prepara shell/catalog sem colocar UI na 08.06 |

**Test-first plan:**

- Primeiro teste novo: `crates/edger-orchestrator/tests/registry_providers.rs` deve falhar porque o registry ainda não registra provider nem valida dependência.
- Comportamentos a provar:
  - registrar provider SQL expõe lookup por `BindingKind::DurableSql`;
  - registrar provider dependente sem SQL falha com erro tipado;
  - registrar dois providers para a mesma capability conflita;
  - worker com binding para provider ausente falha antes do dispatch;
  - inventário admin inclui capabilities declaradas, não strings hardcoded.
- Testes de baixo valor evitados: mocks que apenas provam que `capabilities()` retorna vetor não vazio sem efeito no registry.

### Acceptance criteria
- [x] Crates `edger-ext-*` declaram capabilities sem depender de orchestrator.
- [x] Registry falha com erro claro para capability duplicada incompatível.
- [x] Dependency missing retorna erro tipado em startup ou registro.
- [x] Binding lookup usa provider declarado e namespace.
- [x] Docs explicam diferença entre modelo edger e plugin system Buntime.
- [x] Matriz de valor marca extensão/plugin com evidência.

### Dependencies
- Story 08.02 para inventário operacional.
- Story 08.04 para bindings de serviço quando incluídos.

## Tasks
- [x] Atualizar contrato em `crates/edger-core/src/extension.rs`.
  - Done when: `ExtensionCapability`, `ExtensionDependency` e hooks/capabilities tipados compilam em `edger-core` sem I/O.
- [x] Alinhar traits de provider em `crates/edger-core/src/bindings.rs`.
  - Done when: `DurableSqlProvider`, `KeyValueProvider` e `QueueProvider` também carregam metadata de `Extension`.
- [x] Ajustar registry no orchestrator.
  - Done when: registry registra providers SQL/KV/queue, resolve provider por `BindingKind`, detecta dependência ausente e conflito de provider duplicado.
- [x] Integrar binding lookup ao registry.
  - Done when: worker com `bindings` só recebe `x-edger-bindings` quando o provider requerido existe no registry.
- [x] Declarar capabilities nas extensões existentes.
  - Done when: auth, gateway, turso e keyval expõem capabilities sem depender de `edger-orchestrator`.
- [x] Escrever testes de conflito, dependência e binding/provider lookup.
  - Done when: `cargo test -p edger-orchestrator --test registry_providers` cobre sucesso, conflito, dependência e binding ausente.
- [x] Atualizar docs de extensões e matriz de valor.
  - Done when: docs explicam diferença para Buntime, evidência e lacunas restantes.

## Verification
```bash
cargo test -p edger-orchestrator --test registry_providers
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

### Verification results

- `cargo test -p edger-orchestrator --test registry_providers` — passou; 5 testes.
- `cargo test -p edger-orchestrator --test state_services` — passou; 3 testes.
- `cargo test -p edger-core` — passou.
- `cargo test --workspace` — passou.
- `cargo clippy --workspace -- -D warnings` — passou.
- `cargo fmt -- --check` — passou.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` — passou; 8 epics, 39 stories, 98 refs, 0 missing.
- Runtime local em `PORT=19084`:
  - `/api/admin/extensions` listou `auth`, `gateway`, `keyval` e `turso` com capabilities declaradas.
  - `/state-demo` autenticado recebeu `x-edger-bindings` para `durableSql`, `keyValue` e `queue`.
  - `/state-demo` sem auth retornou `401 Unauthorized`.
