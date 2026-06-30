# Closure — Story 08.06 Extensões, providers e bindings

**Data:** 2026-06-29  
**Story:** `planning/edger/epics/08-valor-buntime/06-modelo-de-extensoes-e-bindings.md`  
**Epic:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Resultado

Story 08.06 concluída. O edger agora tem capabilities e dependências tipadas em `edger-core`, registry com provider slots explícitos, validação de conflito/dependência em startup e binding lookup que exige provider registrado antes de injetar `x-edger-bindings`.

## Entregue

- `ExtensionCapability`, `ExtensionDependency` e `ExtensionHook` em `edger-core`.
- `DurableSqlProvider`, `KeyValueProvider` e `QueueProvider` agora carregam metadata via `Extension`.
- `ExtensionRegistry` registra providers SQL/KV/queue, valida dependências e conflita provider duplicado.
- `edger` registra `edger-ext-turso` e `edger-ext-keyval` no composition root; `EDGER_STATE_DIR` habilita SQLite file-backed.
- `resolve_service_bindings` valida provider por `BindingKind` antes de enviar bindings para workers.
- Extensões existentes declaram capabilities sem depender de `edger-orchestrator`.
- Docs de extensões, operação e matriz de valor atualizados.

## Drift de escopo

- Não houve dynamic loading, marketplace, hot reload completo ou UI de catálogo.
- Menu contribution ficou como capability tipada para shell/catalog futuro, sem UI final.
- O registry v1 não copia o loader topológico do Buntime; ele entrega falha cedo por dependência/conflito no modelo estático do edger.

## Verificação

- `cargo test -p edger-orchestrator --test registry_providers` — passou; 5 testes.
- `cargo test -p edger-orchestrator --test state_services` — passou; 3 testes.
- `cargo test -p edger-core` — passou.
- `cargo test --workspace` — passou.
- `cargo clippy --workspace -- -D warnings` — passou.
- `cargo fmt -- --check` — passou.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh` — passou; 8 epics, 39 stories, 98 refs, 0 missing.
- Evidência runtime: `planning/edger/status/evidence/story-08-06-runtime.txt`.

## Riscos restantes

- Hot reload/enable-disable real ainda depende de refresh/persistência segura.
- Menu/catalog final e marketplace continuam fora do v1.
- SDK worker para executar operações de SQL/KV/queue diretamente ainda é evolução futura sobre os descritores.

## Próximo

Executar 08.07 `planning/edger/epics/08-valor-buntime/07-observabilidade-operacao-e-deploy.md`, fechando saúde, métricas, logs correlacionados e documentação de deploy/backup para preparar a prova final 08.08.
