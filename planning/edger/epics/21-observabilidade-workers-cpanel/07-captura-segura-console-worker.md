# Story 21.07: Captura segura de console dos workers

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** stdout é descartado e stderr só é drenado em falha, portanto o cPanel não possui logs reais do código do worker.
- **Objective:** capturar stdout/stderr de subprocessos persistentes sem bloquear execução, vazar dados ou crescer memória sem limite.
- **Value:** o operador consulta console logs versionados e correlacionáveis junto aos eventos do runtime.
- **Constraints:** drenagem contínua, bounded e fora do hot path; nenhuma captura de bodies/env; comportamento do protocolo UDS preservado.

## Traceability

- **Prototype:** fonte `Worker console` em `OBS-03 Logs explorer` e `OBS-04 Request trace`.
- **Business rules:** identidade `namespace/worker/version/processId`; stream, nível inferido/estruturado, timestamp e mensagem sanitizada; truncamento e drops visíveis.

## Files

| Path | Action | Reason |
|---|---|---|
| `crates/edger-isolation/src/multiproc.rs` | edit | Pipe e drene stdout/stderr continuamente |
| `crates/edger-isolation/src/multiproc_harness.mjs` | inspect/edit | Separar protocolo de logs do worker quando necessário |
| `crates/edger-orchestrator/src/observability.rs` | edit | Normalizar console em eventos bounded |
| `crates/edger-worker/src/pool.rs` | edit | Propagar identidade/processo e lifecycle |
| `crates/edger-isolation/tests/` | create/edit | Volume, linha longa, crash e processo lento |
| `planning/edger/docs/observability.md` | create/edit | Contrato de captura, redaction e limites |

## Detail

### AS-IS

- stdout usa `Stdio::null()`.
- stderr é pipe interno usado para diagnóstico do processo, sem API consultável.

### TO-BE

- Tasks independentes drenam stdout e stderr desde o spawn até o exit.
- Linha recebe limite em bytes, sanitização ANSI/controle, rate limit por processo e enqueue não bloqueante.
- Fila cheia descarta de forma contabilizada, preservando o runtime; nenhum backpressure retorna ao child.
- Logs estruturados opcionais podem fornecer level/requestId/traceId, mas texto arbitrário nunca é interpretado como label.
- Eventos carregam lifecycle do processo e permitem distinguir logs antigos após recycle.

### Scope

- **In:** pipes, drain, envelope, limites, redaction, contadores e testes de carga/falha.
- **Out:** persistência em disco, multiline parser sofisticado, full-text e compatibilidade com formatos de vendors.

### Acceptance criteria

- [x] Worker que escreve além da capacidade não bloqueia nem derruba o processo.
- [x] Linha longa, ANSI, bytes inválidos e secrets conhecidos são truncados/sanitizados.
- [x] Logs são atribuídos à versão e ao processo corretos, inclusive durante recycle.
- [x] Drops/truncamentos aparecem nas estatísticas do store e na UI.
- [x] Caminho sem captura mantém custo e comportamento compatíveis.

### Dependencies

- 21.03 para envelope/store e política de redaction.

## Tasks

- [x] Especificar limites, schema e política de redaction.
- [x] Escrever testes de flood, linha longa, recycle e shutdown.
- [x] Implementar pipe/drain e canal bounded não bloqueante.
- [x] Integrar ao store com identidade/process lifecycle.
- [x] Medir o corte de carga no teste e documentar tuning.

## Verification

```bash
cargo test -p edger-isolation
cargo test -p edger-orchestrator --test observability_api
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
```

## Status

completed (2026-07-11) — captura bounded validada em teste com flood e recycle e no Browser com console real, filtro persistido e redaction de secrets/caminhos.
