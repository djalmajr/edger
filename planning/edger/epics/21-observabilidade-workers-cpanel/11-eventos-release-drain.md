# Story 21.11: Eventos operacionais de release e drain

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** migrations via `release` e cleanup via `beforeunload`/`waitUntil` existem, mas não deixam uma jornada consultável no cPanel; falhas exigem busca em logs do processo.
- **Objective:** publicar eventos tipados e sanitizados de release, drain e termination no store local e correlacioná-los na aba `Logs`.
- **Value:** operador entende por que uma versão não começou a servir, quanto durou a migration e se conexões foram drenadas dentro do grace period.
- **Constraints:** comandos, env, stderr bruto e paths de filesystem não são expostos; eventos são bounded; lifecycle nunca depende de OTEL.

## Traceability

- **Prototype:** `OBS-03 Logs explorer`, `OBS-04 Request trace` e `OBS-06 Worker workspace · Logs`.
- **Business rules:** `release` continua uma fase de deploy, não evento JS; cleanup garantido usa `beforeunload` + `EdgeRuntime.waitUntil`; `onIdle` permanece fora até existir caso de uso aprovado.
- **Source:** `edger-orchestrator/src/deploy.rs`, `edger-isolation/src/multiproc_harness.mjs`, `edger-worker/src/pool.rs`.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/deploy.rs` | edit | Emitir release started/succeeded/failed/skipped com duração |
| `edger-orchestrator/src/observability.rs` | edit | Armazenar eventos tipados bounded |
| `edger-isolation/src/multiproc.rs` | edit | Retornar resultado estruturado do drain |
| `edger-worker/src/pool.rs` | edit | Emitir drain/termination com identidade e causa |
| `edger-orchestrator/src/admin_api.rs` | edit | Consultar eventos por worker/versão/source/kind |
| `workers/cpanel/index.js` | edit | Exibir release/drain na aba `Logs` e detalhe sanitizado |
| `edger-orchestrator/tests/observability_api.rs` | edit | Provar consulta, sanitização e retenção |
| `edger-isolation/tests/uds_roundtrip.rs` | edit | Provar drained count, timeout e motivo |

## Detail

### AS-IS

- `release` roda antes de serving, tem timeout de 300s e marker `.edger-release`; falha impede ativação.
- Shutdown envia `beforeunload`, drena `EdgeRuntime.waitUntil()` dentro de `shutdownGrace` e então termina o processo.
- O pool aguarda dispatch ativo de forma bounded antes de terminar.
- `notify_idle()` no backend multiprocess é no-op; não há contrato `onIdle` no EdgeR atual.

### TO-BE

- Eventos mínimos:
  - `release.started`, `release.succeeded`, `release.failed`, `release.skipped`;
  - `process.drain.started`, `process.drain.completed`, `process.drain.timed_out`;
  - `process.terminated` com causa allowlisted.
- Campos: event ID, timestamp, namespace/name/version, process ID quando existir, duration, outcome, cause, drained promise count e error code sanitizado.
- A aba `Logs` filtra `source=runtime lifecycle` e abre detalhes sem revelar comando, env, stderr bruto ou path local.
- `release.failed` torna a versão `Not ready`/`Release failed`; nunca `Default` ou `Healthy`.

### Scope

- **In:** eventos locais, API, UI, correlação e provas de failure/timeout.
- **Out:** executar migrations por evento JS, retry automático de migration, lock distribuído e evento `idle`.

### Acceptance criteria

- [x] Release bem-sucedido/falho produz sequência consultável com duração e identidade.
- [x] Falha de release não cria marker nem deixa a versão servir.
- [x] Drain registra promises aguardadas, timeout e causa de termination.
- [x] Nenhum payload expõe comando, env, secret, stderr bruto ou filesystem path.
- [x] Eventos aparecem no cPanel sem Collector/OTLP.
- [x] Shutdown continua bounded mesmo com promise que nunca resolve.

### Dependencies

- 21.03 para store/envelope e 21.05 para explorador/detalhe.

## Test-first plan

- **First failing test:** release que retorna exit 3 produz `release.started` + `release.failed`, sem marker, e a API não retorna comando/stderr bruto.
- **Preferred level:** unit/integration para release e drain; API para redaction; Browser para logs/detalhe.
- **Avoid:** testar implementação interna de shell ou timers sem validar resultado operacional.

## Tasks

- [x] Definir schema e allowlist dos eventos. **Done when:** fontes, causas e campos sensíveis estão explícitos.
- [x] Escrever testes de release success/failure/skip. **Done when:** marker, ativação e eventos são coerentes.
- [x] Estruturar resultado de drain. **Done when:** completed/timeout/drained count chegam ao pool.
- [x] Emitir eventos no store bounded. **Done when:** lifecycle funciona com OTEL desligado.
- [x] Expor filtros na Admin API. **Done when:** identidade/source/kind/cursor estão cobertos.
- [x] Renderizar eventos e detalhe no cPanel. **Done when:** migration e cleanup são diagnosticáveis pela interface.
- [x] Validar shutdown real. **Done when:** teste com promise curta e promise infinita prova o teto.

## Verification

```bash
cargo test -p edger-orchestrator --lib run_release_
cargo test -p edger-isolation --features multiproc --test uds_roundtrip graceful_shutdown_dispatches_beforeunload_and_drains_wait_until -- --exact
cargo test -p edger-orchestrator --test observability_api
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check
```

## Status

completed (2026-07-11) — release success/failure/skip e drain completed/timeout/terminated usam o store local; fixture e Browser provaram migration, TTL, beforeunload, waitUntil, process ID e filtros sem OTEL.
