# Epic 03: Isolação e Execução (Spike + Básico)

**Origin:** `planning/edger/roadmap.md` (Fase 3), `planning/edger/design.md` (PR 2, PR 5)

## Traceability
- **Source docs:** `planning/edger/design.md` (Execution Isolation Layer, Embedding Spike Recommendation, PR Plan 2+5), `planning/edger/roadmap.md` (Fase 3), `planning/edger/analysis-synthesis.md` (testes de integração, boundaries)
- **Roadmap phase:** Fase 3 — Camada de Isolação e Execução (Spike + Básico)
- **Depends on epic:** `planning/edger/epics/02-edger-core/00-overview.md` (tipos, traits `Isolate`, wire formats)

## Context

### Problema macro
O runtime precisa executar código de usuário (JS/TS, Wasm, SPA estática) de forma isolada, com limites de recursos e contratos estáveis na fronteira isolate/orquestrador. Sem um spike de embedding, o risco de subestimar complexidade V8/deno_core é alto; sem trait + mock, o WorkerPool (Epic 04) não pode integrar.

### Objetivo da iniciativa
Validar embedding via spike time-boxed (deno_core + facade primário; wasmtime para Wasm), implementar contrato `Isolate` completo em `edger-isolation` com backend mock, preparar wire handling, stubs de limites e estrutura dual-backend (deno facade + wasmtime WASI).

### Resultado esperado
- Documento `spike.md` com go/no-go e recomendações de módulos
- `edger-isolation` com trait implementado (mock), todos os `ExecutionKind` exercitados em testes
- Stubs de limites de recursos e preparação multi-processo (tipos wire reutilizáveis)
- Módulos esqueleto `deno` facade + `wasmtime` prep sem execução de produção ainda
- `cargo test -p edger-isolation` verde; `bun test` inalterado

### Restrições
- Spike time-boxed (não commitar embedding de produção neste epic)
- JS/TS: `deno_core` + facade (decisão do usuário); Wasm: `wasmtime` + WASI standalone (não co-localizado no isolate JS)
- Multi-processo desde cedo: wire formats de `edger-core` como contrato IPC futuro
- `edger-isolation` depende apenas de `edger-core` (sem `edger-worker` / `edger-orchestrator`)
- Disciplina: `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check`

### AS-IS
- `edger-isolation/` contém apenas `Cargo.toml` stub ou `lib.rs` mínimo
- Trait `Isolate` pode existir apenas como assinatura em `edger-core` (Epic 02)
- Sem spike documentado, sem mock backend, sem limites

### TO-BE
- Spike concluído com métricas baseline (spawn, exec, memória) e comparação wasmtime
- `edger-isolation` com módulos: `isolate`, `mock`, `limits`, `wire`, `error`, `kinds`, stubs `deno`/`wasm`
- Mock `Isolate` cobrindo `execute_fetch`, `execute_routes`, `serve_static_spa`, `execute_wasm`, `notify_idle`, `terminate`
- Hooks de eszip/precomp documentados (não implementação completa)

### Fora de escopo
- Execução JS/TS de produção (Epic posterior / PR 10 do design)
- Integração real com WorkerPool (Epic 04 usa mock via trait)
- Orquestrador HTTP, auth, extensões
- Clustering multi-processo completo (apenas prep de wire/transport)

## Story backlog

| Story | Arquivo | Tamanho | Status | Depende de |
|---|---|---|---|---|
| 03.01 Embedding spike | `01-embedding-spike.md` | large | **completed** | Epic 02 (parcial: wire types) |
| 03.02 Isolate trait impl | `02-isolate-trait-impl.md` | large | **completed** | 03.01, Epic 02.04 |
| 03.03 Wire + limites | `03-wire-limits.md` | medium | **completed** | 03.02, Epic 02.03 |
| 03.04 Dual-backend prep | `04-dual-backend-prep.md` | medium | not started | 03.01, 03.02, 03.03 |

## Epic roadmap

```mermaid
flowchart LR
    S01[03.01 Spike] --> S02[03.02 Trait + mock]
    S02 --> S03[03.03 Wire + limites]
    S01 --> S04[03.04 Dual-backend prep]
    S03 --> S04
```

## Epic acceptance criteria
- [ ] `planning/edger/epics/03-isolacao-execucao/spike.md` com Go/no-go e métricas preenchidos (skeleton existe no backlog)
- [ ] `edger-isolation` depende somente de `edger-core`
- [ ] Mock `Isolate` passa testes para todos os variantes de `ExecutionKind`
- [ ] Stubs `ResourceLimits` + transport prep documentados para multi-processo
- [ ] Módulos `deno` (facade skeleton) e `wasm` (wasmtime WASI skeleton) compilam sob feature flags
- [ ] `cargo test -p edger-isolation` e gate workspace verdes
- [ ] `bun test` continua passando

## Risks

| Risco | Severidade | Mitigação |
|---|---|---|
| Spike revela custo V8/deno_core maior que previsto | Alta | Time-box; fallback prolongado em mocks; spike.md com plano B |
| Divergência trait core vs impl isolation | Média | Re-exportar trait de core; testes de compilação cruzada |
| Multi-processo adiciona complexidade cedo | Média | Apenas wire + framing stub; in-process como default dev |
| Manutenção wasmtime vs deno_core em paralelo | Média | Backends separados por módulo/feature; decisão usuário já fixada |

## Próximo passo recomendado
`/agile-story` em `01-embedding-spike.md` assim que Epic 02 stories 02.03 e 02.04 estiverem estáveis (wire + trait signatures).

## Status
ready-for-development (planning complete; implementação bloqueada por Epic 02)