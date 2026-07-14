# Story 17.E: Remover o sistema de extensões/registry/hooks

**Origin:** `planning/edger/epics/17-edger-minimalista/00-overview.md`

## Context

- **Problema:** `ExtensionRegistry` + trait `Extension`/`Middleware` + hooks `onRequest`/`onResponse`/lifecycle existem para plugar auth, providers de estado e gateway. Removidos esses (17.A–17.D), **sobra zero consumidor legítimo**. Observabilidade (request-id, métricas, tracing) já é middleware built-in, não hook.
- **Objetivo:** deletar o sistema de extensões inteiro; limpar vestígios de `visibility` e `namespaces` (só existiam para gate/bindings).
- **Valor:** remove a maior camada de indireção do projeto; o edger vira um binário direto sem plugin machinery.
- **Restrições:** preservar a observabilidade built-in intacta; se algum dia precisar de hook, redesenha para o caso real (git history preserva o atual).

## Traceability
- `crates/edger-core/src/extension.rs`, `crates/edger-orchestrator/src/registry.rs`, `pipeline.rs` (`run_on_request`/`run_on_response`/`run_on_worker_*`); `config.rs`/`manifest.rs` (`visibility`, `namespaces`, `publicRoutes`)

## Files
| Path | Action | Reason |
|---|---|---|
| `crates/edger-core/src/extension.rs` | delete | Trait `Extension`/`Middleware`/capabilities sem consumidor |
| `crates/edger-orchestrator/src/registry.rs` | delete | Registry sem nada a registrar |
| `crates/edger-orchestrator/src/pipeline.rs` | edit | Remover chamadas de hook (`run_on_*`) do dispatch |
| `crates/edger-core/src/config.rs`, `manifest.rs` | edit | Remover `visibility`, `namespaces`, `publicRoutes` |
| `crates/edger-orchestrator/src/admin_api.rs` | edit | Remover `/api/admin/extensions*` |

## Detail
### Scope
- **In:** deletar `Extension`/`Middleware`/`ExtensionRegistry`/hooks; remover `visibility`/`namespaces`/`publicRoutes`; remover inventário de extensões do admin/cPanel.
- **Out:** qualquer novo sistema de plugins (YAGNI; redesenhar quando houver caso real).

### Acceptance criteria
- [x] `Extension`/`Middleware`/`ExtensionRegistry`/hooks deletados; `pipeline` despacha sem `run_on_*`; workspace compila.
- [x] Observabilidade preservada: `/metrics`, `/metrics/stats`, request-id e tracing seguem funcionando (suites verdes; live fica para o coordenador fora do sandbox).
- [x] `visibility`/`namespaces`/`publicRoutes` removidos do manifest/config; manifests existentes carregam (campos ignorados/removidos).
- [x] `/api/admin/extensions*` removido.

### Dependencies
- Stories 17.A–17.D (todos os consumidores do registry precisam sair antes)

## Tasks
- [x] Remover chamadas de hook do pipeline; deletar `extension.rs`/`registry.rs`; limpar manifest/config.
- [x] Confirmar observabilidade built-in intacta por teste (preview/live fora do sandbox).

## Verification
```bash
cargo test --workspace
cargo test -p edger-orchestrator --test metrics_endpoint
```

## Completion

Concluída em 2026-07-03. Gates locais executados: `cargo fmt --all --check`,
`cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets`
com contagem `warning|error = 0`, `cargo test -p edger-orchestrator --lib`,
`cargo test -p edger-core`, e o conjunto focado de testes admin/pipeline/routing/metrics.
