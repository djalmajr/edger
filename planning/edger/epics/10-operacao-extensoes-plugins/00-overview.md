# Epic 10: Operacao de Extensoes e Plugins

**Origin:** `planning/edger/roadmap.md`

**Depends on epic:** `planning/edger/epics/06-extensibilidade/00-overview.md`

## Context

### Macro problem

O Epic 08 provou registry estatico, providers, hooks, capabilities e enable/disable operacional de extensoes. O Buntime, porem, trata plugins como unidades operaveis com manifesto, menus, diagnosticos, dependencias e ciclo de vida. O edger precisa entregar esse valor sem dynamic loading de crates Rust e sem empurrar estado operacional para `edger-core`.

### Initiative objective

Criar a camada operacional de modulos/extensoes: inventario machine-readable, manifesto persistente, reconcile/reload controlado e validacao local. O resultado deve permitir que um operador ou agente descubra quais capacidades existem, qual estado esta ativo e quais passos locais sao seguros antes de alterar runtime.

### AS-IS

- `edger-core` contem contratos puros para capabilities, hooks e providers.
- `edger-orchestrator` registra extensoes por lista explicita no composition root.
- `ExtensionRegistry` ja suporta enable/disable runtime, status store opcional e diagnostics.
- A matriz aponta reload/rescan dinamico e manifesto completo como lacunas.

### TO-BE

- Modulos tem inventario operacional com status, origem, capacidades, dependencias, diagnostics e redaction de configuracao.
- Reconcile diferencia dry-run, reload seguro de configuracao e mudancas que exigem restart.
- Manifesto operacional persiste informacoes de modulo sem virar loader dinamico de Rust.
- Validacao local de extensoes roda gates de planejamento/codigo e captura evidencia versionada.

### Out of scope

- Dynamic loading de crates Rust em runtime.
- Publicacao manual em crates.io.
- Marketplace publico.
- Mover providers externos para o core.

## Story backlog

| Story | Arquivo | Objetivo | Tamanho | Status | Depende de |
|---|---|---|---|---|---|
| 10.01 Inventario operacional | `01-inventario-operacional-de-modulos.md` | Definir e expor inventario de modulos/extensoes com capabilities e diagnostics | medium | completed | Epic 06, Epic 08.13 |
| 10.02 Reconcile/reload controlado | `02-reconcile-reload-controlado.md` | Separar dry-run, reconcile e reload seguro sem loader dinamico de Rust | large | planned | 10.01, Epic 08.26 |
| 10.03 Manifesto operacional | `03-manifesto-configuracao-modulo.md` | Persistir manifesto/configuracao de modulo com redaction e compatibilidade | medium | completed | 10.01 |
| 10.04 Validacao local de extensoes | `04-validacao-local-extensoes.md` | Rodar validacao local de modulo/extensao e registrar evidencia | medium | planned | 10.02, 10.03 |

## Epic acceptance criteria

- [ ] Inventario operacional lista modulos, origem, capabilities, dependencias, status, diagnostics e configuracao segura.
- [ ] Reconcile/reload diferencia o que pode mudar em runtime do que exige restart.
- [ ] Manifesto operacional persiste estado suficiente para auditoria local sem loader dinamico.
- [ ] Validacao local prova pelo menos uma extensao existente com gates e evidencia versionada.
- [ ] Nenhuma implementacao coloca I/O em `edger-core`.
- [ ] Gate de planejamento fica verde: `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`.

## Status

in-progress (2026-07-01) - Stories 10.01 e 10.03 entregues. 10.01: inventario operacional root-only expoe `id`, `version`, `kind`, `capabilities`, `dependencies`, `status`, `configSource` e diagnostics redigidos (chaves/valores/paths sensiveis) via `/api/admin/extensions`, com testes cobrindo middleware (gateway) e provider (keyval) e negacao non-root. 10.03: manifesto operacional tipado por extensao (`manifest` com `menus`, `hooks`, `provides`, `requirements` e `config` com `keys` por nome + `redacted`/`source`), derivado das capabilities/dependencias declaradas, backward-compatible com registro estatico, documentado no adoc e coberto em `registry_providers.rs`. Reconcile/reload e validacao local seguem nas stories 10.02 e 10.04.

planned (2026-06-29) - criado para retirar operacao de plugins/extensoes do Epic 08 e dar dono modular para reload/reconcile, manifesto operacional, diagnostics e validacao local.

