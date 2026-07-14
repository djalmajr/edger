# Story 08.28: Hooks de lifecycle de worker

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- Problema atual: a matriz ainda marca `Hooks request/response/lifecycle` como `must partial`; request/response, init e shutdown já existem, mas o lifecycle de worker ainda não tem contrato executável.
- Objetivo de entrega: expor hooks de lifecycle de worker no contrato de extensão e provar que eles envolvem o dispatch real pelo `WorkerPool`.
- Restrições: não acoplar `edger-worker` a extensões, não criar loader dinâmico, não reabrir o modelo de registro explícito v1 e não transformar lifecycle em mutação de resposta duplicada de `on_response`.
- Referências: `crates/edger-core/src/extension.rs`, `crates/edger-orchestrator/src/hooks.rs`, `crates/edger-orchestrator/src/pipeline.rs`, `crates/edger-orchestrator/tests/registry_hooks.rs`, `planning/edger/docs/value-parity-matrix.md`.

## Traceability
- Protótipos/telas: não aplicável.
- Regras de negócio: extensões precisam observar início, sucesso e erro de dispatch de worker sem executar código de app no processo principal.
- Source docs: `planning/edger/epics/08-valor-buntime/06-modelo-de-extensoes-e-bindings.md`, `planning/edger/epics/08-valor-buntime/13-extension-enable-disable-runtime.md`, `planning/edger/docs/value-parity-matrix.md`.

## Files

| Arquivo | Ação | Motivo | Confiança |
|---|---|---|---|
| `crates/edger-core/src/extension.rs` | Alterar | Adicionar capacidades e métodos default para lifecycle de worker | core |
| `crates/edger-orchestrator/src/hooks.rs` | Alterar | Executar hooks de lifecycle em ordem controlada | core |
| `crates/edger-orchestrator/src/pipeline.rs` | Alterar | Disparar hooks ao redor do dispatch real pelo `WorkerPool` | core |
| `crates/edger-orchestrator/tests/registry_hooks.rs` | Alterar | Provar lifecycle em request real e skip em short-circuit | core |
| `planning/edger/docs/value-parity-matrix.md` | Alterar | Marcar a linha de hooks como testada quando houver evidência | core |
| `planning/edger/epics/08-valor-buntime/00-overview.md` | Alterar | Registrar Story 08.28 no backlog/status | core |
| `planning/edger/roadmap.md` | Alterar | Atualizar contagem da Fase 8 | core |
| `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md` | Alterar | Atualizar checkpoint de valor e lacunas restantes | core |
| `planning/edger/status/evidence/story-08-28-runtime.txt` | Criar | Registrar comandos reais e resultado da entrega | core |

## Detail

### Estado atual (AS-IS)
- `Middleware` tem `on_request` e `on_response`.
- `Extension` tem `on_init`, `on_server_start` e `on_shutdown`.
- O pipeline chama request/response hooks e depois faz `WorkerPool::fetch_worker`.
- Não há evento tipado que prove início, conclusão ou erro de dispatch de worker.

### Estado alvo (TO-BE)
- `ExtensionHook` inclui labels para `onWorkerDispatch`, `onWorkerComplete` e `onWorkerError`.
- `Middleware` ganha métodos default de lifecycle de worker, mantendo compatibilidade com extensões existentes.
- O orchestrator executa lifecycle hooks somente em requests que chegam ao dispatch real de worker.
- Short-circuit de `on_request` não dispara lifecycle de worker.
- A matriz pode marcar `Hooks request/response/lifecycle` como `tested`.

### Escopo
- Inclui contrato Rust e teste de pipeline.
- Inclui evento de erro de dispatch antes de retornar erro para o pipeline.
- Não inclui loader dinâmico, plugin marketplace, lifecycle interno do isolate ou métricas persistentes.

### Approach
- Adicionar métodos default ao trait `Middleware`, preservando implementações existentes.
- Implementar helpers em `hooks.rs`: dispatch, complete e error.
- Chamar os helpers em `pipeline.rs` ao redor de `state.pool.fetch_worker`.
- Escrever testes que observam a ordem dos eventos e garantem que short-circuit não chama lifecycle.

### Risks and dependencies
- Risco: lifecycle virar duplicação de `on_response`. Mitigação: complete/error são observacionais; mutação de resposta continua em `on_response`.
- Risco: acoplamento indevido ao worker crate. Mitigação: hooks vivem em `edger-core`/`edger-orchestrator`, usando `RequestContext`.

## Acceptance criteria
- [x] `Middleware` expõe hooks default para dispatch, complete e error de worker.
- [x] O pipeline chama lifecycle hooks somente quando há dispatch real ao `WorkerPool`.
- [x] Short-circuit de request hook não dispara lifecycle de worker.
- [x] A linha `Hooks request/response/lifecycle` fica `tested` na matriz com evidência de teste.

## Test-first plan
- Comportamento a provar: middleware de lifecycle observa request real na ordem request -> worker dispatch -> worker complete -> response.
- Primeiro teste falhando: adicionar teste de pipeline esperando eventos de lifecycle antes dos métodos existirem.
- Nível preferido: integração do orchestrator com `build_pipeline`.
- Valor do teste: contrato de extensão e fronteira de execução.
- Testes de baixo valor a evitar: asserts de método definido sem provar efeito no pipeline.

## Tasks
- [x] Adicionar contrato core de lifecycle. **Done when:** labels e métodos default compilam sem alterar extensões existentes.
- [x] Integrar helpers no orchestrator. **Done when:** dispatch/complete/error rodam em ordem e respeitam enable/disable.
- [x] Cobrir pipeline com testes. **Done when:** há teste de sucesso e short-circuit protegendo lifecycle.
- [x] Atualizar artefatos de paridade. **Done when:** matriz, overview, roadmap e checkpoint apontam Story 08.28.
- [x] Registrar evidência e closure. **Done when:** evidence/closure citam comandos reais.
- [x] Rodar verificação. **Done when:** Rust gate e planning gate passam.

## Verification
- [x] `cargo test -p edger-orchestrator --test registry_hooks`
- [x] `cargo test --workspace`
- [x] `cargo clippy --workspace -- -D warnings`
- [x] `cargo fmt -- --check`
- [x] `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`

## Recommended next step
- Continuar a próxima linha `must partial` da matriz.
