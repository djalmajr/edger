# Extensões edger (edger-ext-*)

**Status:** PLANNING SKELETON — preencher no Epic 06  
**Origin:** `planning/edger/epics/06-extensibilidade/00-overview.md`

## Princípio choose ONE

Cada crate `edger-ext-*` escolhe **um** modo por crate:

- **Middleware** (hooks `on_request` / `on_response`), ou
- **WorkerHandler** / provider especializado (ex: `AuthProvider`)

Nunca duplicar ambos modos na mesma crate.

## Padrão de registro (story 06.01)

- Opções avaliadas: `inventory`, `linkme`, registro explícito no bin
- Decisão: _a preencher na story 06.01_
- Ordem de execução: priority ou topo sort (paridade Buntime)

## Checklist nova extensão

- [ ] Crate `edger-ext-<nome>` depende apenas de `edger-core` (traits definidos em core)
- [ ] Implementa trait documentado em `edger-core`
- [ ] Registro estático no bin `edger`
- [ ] `cargo test -p edger-ext-<nome>` verde
- [ ] Sem I/O no core; extensão pode ter store próprio

## Walkthrough edger-ext-auth (story 06.02)

_Preencher após implementação._

## Template edger-ext-gateway (story 06.03)

_Preencher após implementação._