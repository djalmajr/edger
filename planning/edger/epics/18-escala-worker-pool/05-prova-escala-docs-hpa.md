# Story 18.E: Prova de escala + docs L1/L2

**Origin:** `planning/edger/epics/18-escala-worker-pool/00-overview.md`

## Context

- **Problema:** a melhoria de escala precisa ser comprovada contra o baseline real. O repo já tem `crates/edger-orchestrator/tests/perf_harness.rs`, mas ele mede warm-hit sequencial e espera `active_workers = 1`; não compara 1 vs N processos sob concorrência nem orienta o operador sobre quando usar pool interno versus HPA.
- **Objetivo:** estender o harness para comparar `maxProcesses: 1` vs `N` sob concorrência; documentar Level 1 (pool interno) e Level 2 (HPA do chart existente); declarar Level 3 (Knative/FaaS) fora de escopo.
- **Valor:** evita "escala por fé": o operador terá números, comandos e docs que separam concorrência intra-réplica de capacidade por réplicas.
- **Restrições:** perf harness segue ignorado por padrão e executado explicitamente; docs não prometem autoscaling que o chart não implementa.

## Traceability

- `crates/edger-orchestrator/tests/perf_harness.rs` (`persistent_worker_warm_hit_baseline`)
- `planning/edger/docs/performance-baselines.md`
- `planning/edger/docs/deployment-k8s.md`
- `charts/edger/values.yaml` (`hpa.enabled`, `minReplicas`, `maxReplicas`, target CPU)
- `charts/edger/templates/hpa.yaml` (`HorizontalPodAutoscaler autoscaling/v2`)
- `charts/edger/questions.yaml` (grupo Scaling)
- `planning/edger/epics/17-edger-minimalista/06-deployment-k8s.md`

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-orchestrator/tests/perf_harness.rs` | edit | Adicionar cenários concorrentes 1 vs N processos e imprimir p50/p95/throughput/wait |
| `planning/edger/docs/performance-baselines.md` | edit | Registrar baseline Level 1 antes/depois com comando reprodutível |
| `planning/edger/docs/deployment-k8s.md` | edit | Documentar Level 2 HPA e como ele se combina com `maxProcesses` |
| `planning/edger/docs/scaling.md` | create | Doc operacional de escala L1/L2/L3 fora de escopo |
| `charts/edger/values.yaml` | edit | Se necessário, adicionar valores/documentação de env/config de pool no chart |
| `charts/edger/questions.yaml` | edit | Se necessário, expor configuração de pool no form Rancher |

## Detail

### AS-IS
- `perf_harness.rs` cria `WorkerPool::with_factory(PoolConfig::default(), Arc::new(PerfFactory))`, executa 50 requests sequenciais e espera 1 cache miss, 49 hits e 1 worker ativo.
- `charts/edger/templates/hpa.yaml` já renderiza `HorizontalPodAutoscaler` quando `hpa.enabled` está ativo.
- `planning/edger/docs/deployment-k8s.md` declara que escala fina de pool por worker e tuning adicional de HPA ficam no Epic 18.
- Não há doc única explicando Level 1/2/3.

### TO-BE
- Harness inclui um isolate lento/controlado e roda cenários concorrentes:
  - `maxProcesses: 1`, concorrência M.
  - `maxProcesses: N`, mesma concorrência M.
  - com e sem fila, quando aplicável.
- Saída imprime cenário, requests, concorrência, `maxProcesses`, p50, p95, throughput, wait/rejeições e processos ativos.
- Docs explicam:
  - Level 1: aumenta concorrência do mesmo worker dentro de uma réplica; consome memória por processo.
  - Level 2: HPA aumenta réplicas do edger; depende de CPU/mem e do roteamento externo; não resolve sozinho head-of-line por worker se cada réplica ainda tem `maxProcesses: 1`.
  - Level 3: Knative/FaaS fica fora de escopo e não deve ser construído neste épico.

### Scope
- **In:** perf harness ignorado; docs operacionais; referência ao chart/HPA existente; atualização de baselines.
- **Out:** benchmark distribuído real em cluster; autoscaling por métrica customizada; instalar Knative; publicar chart.

### Acceptance criteria
- [x] `cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture` imprime comparação 1 vs N sob concorrência.
- [x] O cenário N mostra ganho verificável ou, se não mostrar, imprime wait/rejeições suficientes para diagnosticar o gargalo.
- [x] `performance-baselines.md` registra comando, ambiente e placeholders de números sem apresentar baseline local como garantia universal. Números definitivos: a preencher pelo harness do coordenador fora do sandbox.
- [x] `deployment-k8s.md` e `scaling.md` explicam HPA Level 2 usando `charts/edger/templates/hpa.yaml`, `values.yaml` e `questions.yaml`.
- [x] Docs dizem explicitamente: Level 3 Knative/FaaS é fora de escopo; não construir no Epic 18.
- [x] Chart não expõe configurações de pool: `maxProcesses`/`queueLimit` são por worker no manifesto, não env global. Nenhum campo novo de chart foi criado.

### Dependencies
- Stories 18.A, 18.B, 18.C e 18.D

## Tasks
### Fase 1 — Harness
- [x] Adicionar isolate lento e cenários concorrentes.
- [x] Parametrizar `maxProcesses`/fila no estado de teste.
- [x] Imprimir métricas de comparação em formato grepável.
### Fase 2 — Baseline
- [x] Rodar harness explicitamente no sandbox apenas para provar que os cenários executam e imprimem.
- [x] Atualizar `performance-baselines.md` com comando, placeholders e ressalvas. Números definitivos ficam a preencher pelo coordenador fora do sandbox.
### Fase 3 — Docs de escala
- [x] Criar/atualizar doc L1/L2.
- [x] Referenciar chart HPA existente.
- [x] Declarar L3 fora de escopo.
### Fase 4 — Chart opcional
- [x] Confirmar que campos de pool são por worker no manifesto e não devem virar env/config global do chart.
- [x] Sem alteração no chart; `helm template` não é necessário para esta story porque nenhum template/valor do chart foi modificado.

## Verification

```bash
cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
helm template edger charts/edger
ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
curl -s http://127.0.0.1:19080/metrics | rg "edger_pool|edger_worker|queue|wait"
```

## Status

**completed** (2026-07-03) — harness concorrente 1 vs N implementado e docs L1/L2 atualizados. Números definitivos de baseline ficam a preencher pelo coordenador em execução fora do sandbox; a execução local do agente serve apenas como prova de que os cenários rodam e imprimem linhas grepáveis.
