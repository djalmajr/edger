# Story 21.10: Health passivo e vocabulário operacional

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** `Serving` hoje significa apenas versão habilitada/default, mas a UI o apresenta como se provasse disponibilidade. Um worker pode falhar em todas as execuções, ser reciclado e continuar aparecendo como `Serving`.
- **Objective:** separar roteamento, processo e confiabilidade recente, calculando health passivo por worker/versão a partir do tráfego real.
- **Value:** o operador distingue app roteável de app saudável sem criar probes que aquecem workers serverless.
- **Constraints:** identidade `namespace + name + version`; janela e reset explícitos; memória bounded; HTTP 4xx não degrada health por padrão; nenhuma mensagem vira label Prometheus.

## Traceability

- **Prototype:** atualizar `OBS-01 Observability overview`, `OBS-02 Worker detail` e `OBS-06 Worker workspace · Logs` no Paper.
- **Business rules:** `Default`/`Enabled`/`Disabled` são estados administrativos; `Cold` não é falha; ausência de tráfego é `Unobserved`; OTEL não é fonte do cPanel.
- **Source:** `/metrics/stats`, `WorkerErrorLog`, métricas do pool e Epic 21.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-worker/src/metrics.rs` | edit | Janela bounded, outcomes e falhas consecutivas por identidade |
| `edger-worker/src/pool.rs` | edit | Registrar sucesso, HTTP 5xx, erro de isolate, timeout e rejeição |
| `edger-orchestrator/src/metrics.rs` | edit | Serializar health, janela, last success/failure e freshness |
| `workers/cpanel/index.js` | edit | Trocar `Serving`, mostrar as três dimensões e filtros corretos |
| `edger-worker/tests/integration_pool.rs` | edit | Provar transições de health com tráfego real |
| `edger-orchestrator/tests/metrics_endpoint.rs` | edit | Provar contrato JSON e reset/freshness |
| `planning/edger/scripts/cpanel-ui-gate.sh` | edit | Provar estados desktop/mobile e worker que falha propositalmente |

## Detail

### AS-IS

- A contagem `Serving` usa todas as versões não desabilitadas.
- A linha default recebe status `Serving`; versões não-default recebem `Enabled`.
- `Needs attention` deriva de disabled ou erro recente agregado apenas por nome.
- `unhealthy` descreve uma instância atual; processos que falham são removidos e esse flag não representa confiabilidade histórica.

### TO-BE

- **Routing:** `Default`, `Enabled`, `Disabled`.
- **Process:** `Cold`, `Idle`, `Active`, `Queued`, `Terminating`.
- **Health:** `Unobserved`, `Healthy`, `Degraded`, `Failing`.
- Janela inicial de cinco minutos, declarada na API/UI:
  - `Unobserved`: nenhuma execução na janela;
  - `Healthy`: há sucesso recente e nenhum erro/timeout/rejeição na janela;
  - `Degraded`: há sucesso, mas também erro, timeout, rejeição ou pressão de fila;
  - `Failing`: três falhas consecutivas, circuit breaker aberto ou nenhuma execução bem-sucedida após pelo menos três tentativas.
- HTTP 5xx, erro de isolate e timeout contam como falha; HTTP 4xx permanece outcome de aplicação/cliente, não health failure por padrão.
- O detalhe exibe janela, amostras, taxa de erro, falhas consecutivas, último sucesso, última falha e código sanitizado.

### Scope

- **In:** health passivo em memória, API, cPanel, filtros e evidência com tráfego real.
- **Out:** probe periódico, SLA/SLO, retenção longa, alerting e health distribuído entre réplicas.

### Acceptance criteria

- [x] Worker sem tráfego aparece `Unobserved`, mesmo quando default e habilitado.
- [x] `boom-ui` aparece `Default` + estado de processo + `Failing` após três respostas HTTP 500 reais.
- [x] `cpanel-scenario` aparece `Degraded` após sucesso + falha; `commonjs` aparece `Healthy` após somente sucessos.
- [x] Reinício/reset e freshness ficam explícitos e não reutilizam amostras antigas.
- [x] Filtros distinguem routing status de health status.
- [x] Layout permanece responsivo sem scroll horizontal interno.

### Dependencies

- 21.03 para envelope/identidade; pode começar pelo contrato de métricas existente antes do store de eventos completo.

## Test-first plan

- **First failing test:** sequência `success → 5xx → isolation error` produz `Degraded`, depois três falhas consecutivas produzem `Failing` para a mesma versão.
- **Preferred level:** unit no agregador bounded, integração no pool/API e Browser com tráfego real.
- **Avoid:** snapshots de classes Tailwind ou testes que apenas repetem strings sem validar semântica.

## Tasks

- [x] Especificar o DTO de health e os thresholds iniciais. **Done when:** contrato distingue routing/process/health e declara janela/reset.
- [x] Escrever testes do agregador e do pool. **Done when:** success, 5xx, timeout, rejeição, isolate error e reset estão cobertos.
- [x] Implementar janela bounded por identidade. **Done when:** cardinalidade e retenção têm limites explícitos.
- [x] Expor o contrato em `/metrics/stats`. **Done when:** JSON inclui freshness e não contém dados sensíveis.
- [x] Renomear `Serving` para `Default`/`Enabled` conforme o contexto. **Done when:** nenhuma tela usa routing como sinônimo de health.
- [x] Implementar indicadores e filtros de health. **Done when:** os quatro estados são visíveis e acessíveis.
- [x] Gerar tráfego real de sucesso/falha. **Done when:** Browser prova `Unobserved`, `Healthy`, `Degraded` e `Failing`.

## Verification

```bash
cargo test -p edger-worker --test integration_pool
cargo test -p edger-orchestrator --test metrics_endpoint
planning/edger/scripts/cpanel-scenario.sh setup
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check
```

Validação manual obrigatória no Browser em desktop e viewport responsivo, incluindo refresh e deep link do worker/versão.

## Status

completed (2026-07-11) — health passivo de cinco minutos e até 64 amostras por identidade implementado no pool, exposto em `/metrics/stats` e integrado ao cPanel com filtros independentes de routing/health. Evidência: `planning/edger/status/evidence/worker-passive-health-2026-07-11.md`.
