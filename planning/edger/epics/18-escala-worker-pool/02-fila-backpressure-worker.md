# Story 18.B: Fila e backpressure por worker

**Origin:** `planning/edger/epics/18-escala-worker-pool/00-overview.md`

## Context

- **Problema:** depois que o worker tiver N processos, ainda é necessário definir saturação. Hoje há fila limitada apenas para workers efêmeros (`ttl_ms == 0`) via `EphemeralGate`; workers persistentes bloqueiam no `dispatch_lock` da instância e não expõem timeout de fila nem resposta tipada para excesso.
- **Objetivo:** criar fila limitada por worker persistente quando todos os processos do grupo estiverem ocupados, com timeout de espera, erro tipado e mapeamento HTTP claro (429/503). Streams longos devem ocupar só sua instância e não travar a capacidade inteira enquanto houver processos livres.
- **Valor:** backpressure explícito protege a réplica e dá sinal operacional para cliente/HPA/API Gateway, em vez de transformar concorrência em espera invisível ou 500 genérico.
- **Restrições:** fila é em memória, por réplica e por worker; não é queue persistente, retry broker, nem mecanismo de ordering garantido.

## Traceability

- `edger-worker/src/ephemeral.rs` (`EphemeralGate`, semáforo + queue_limit para `ttl_ms == 0`)
- `edger-worker/src/error.rs` (`WorkerError::EphemeralQueueFull`; falta erro persistente tipado)
- `edger-worker/src/pool.rs` (`fetch_worker_inner`, `fetch_worker_stream_inner`, locks e cancel safety)
- `edger-orchestrator/src/pipeline.rs` (`worker_error_to_core`, `map_error_status` hoje mapeia `WORKER_ERROR` para 500)
- `edger-worker/tests/metrics_ephemeral.rs` (`ephemeral_queue_full_returns_typed_error`)
- `edger-worker/tests/cancel_safety.rs` (request cancelado recicla instância)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/manifest.rs` | edit | Adicionar `queueLimit`/`queueTimeout` ou nomes equivalentes ao contrato do worker |
| `edger-core/src/config.rs` | edit | Normalizar fila/timeout em `WorkerConfig` |
| `edger-worker/src/types.rs` | edit | Incluir configuração de fila persistente por worker/grupo |
| `edger-worker/src/error.rs` | edit | Adicionar erros tipados (`WorkerQueueFull`, `WorkerQueueTimeout`, saturação/shutdown) |
| `edger-worker/src/pool.rs` | edit | Implementar fila bounded por grupo e aquisição com timeout |
| `edger-worker/src/metrics.rs` | edit | Preparar contadores de fila/rejeição/latência para 18.D |
| `edger-orchestrator/src/pipeline.rs` | edit | Mapear erros tipados para 429/503 e body JSON consistente |
| `edger-worker/tests/metrics_ephemeral.rs` | edit | Reusar padrões de teste do gate, sem confundir ephemeral com persistente |
| `edger-orchestrator/tests/pipeline_integration.rs` | edit | Provar status HTTP tipado sob saturação |

## Detail

### AS-IS
- `EphemeralGate` tem `ephemeral_concurrency` e `ephemeral_queue_limit`.
- Workers persistentes usam o `dispatch_lock` da instância; concorrentes esperam no lock sem timeout explícito.
- `worker_error_to_core` embrulha qualquer `WorkerError` como `CoreError::new("WORKER_ERROR", ...)`, e `map_error_status` cai em 500 para códigos desconhecidos.
- Streaming mantém o processo ativo até o fim do body; disconnect recicla a instância, mas enquanto o stream está aberto a instância fica ocupada.

### TO-BE
- Cada grupo de worker persistente tem uma fila bounded para requests que chegam quando todos os processos estão ocupados e `maxProcesses` já foi atingido.
- Configuração sugerida em manifesto:
  - `queueLimit`: número máximo de requests esperando por worker por réplica, default conservador.
  - `queueTimeout`: tempo máximo de espera antes de rejeitar, default curto e explícito.
- Saturação imediata por fila cheia retorna erro tipado mapeado para 429 ou 503, com código estável no body.
- Timeout de espera retorna erro tipado mapeado para 503.
- Stream longo ocupa apenas o processo que está servindo o stream; outros processos do mesmo worker continuam disponíveis.

### Scope
- **In:** fila in-memory por worker persistente; timeout de fila; erros tipados; status HTTP; testes de saturação e stream longo.
- **Out:** retry automático; persistência da fila; ordenação global entre réplicas; prioridade por rota; integração com API Gateway externo.

### Acceptance criteria
- [x] `queueLimit: 0` rejeita imediatamente quando todos os processos do worker estão ocupados e `maxProcesses` foi atingido. (Concluído em 2026-07-03)
- [x] `queueLimit: 1` permite exatamente 1 request esperando; o próximo recebe erro tipado sem entrar no lock indefinidamente. (Concluído em 2026-07-03)
- [x] `queueTimeout` expira request que esperou demais e retorna status HTTP documentado (503 recomendado para timeout de capacidade). (Concluído em 2026-07-03)
- [x] Fila cheia retorna status HTTP documentado (429 se tratado como rate/backpressure do worker; 503 se tratado como capacidade indisponível), com código estável no body. (Concluído em 2026-07-03)
- [x] Stream/SSE longo em uma instância não bloqueia requests que possam usar outra instância livre do mesmo grupo. (Concluído em 2026-07-03)
- [x] Cancelamento de request esperando na fila remove o waiter e não vaza contadores/permissões. (Concluído em 2026-07-03)

### Dependencies
- Story 18.A

## Tasks
### Fase 1 — Contrato de saturação
- [x] Definir nomes finais dos campos (`queueLimit`, `queueTimeout`) em manifesto/config. (Concluído em 2026-07-03)
- [x] Adicionar erros persistentes tipados em `WorkerError`. (Concluído em 2026-07-03)
- [x] Definir mapeamento HTTP e shape do body. (Concluído em 2026-07-03)
### Fase 2 — Fila bounded
- [x] Implementar aquisição de slot por grupo com timeout. (Concluído em 2026-07-03)
- [x] Garantir cancel safety para waiters. (Concluído em 2026-07-03)
- [x] Integrar com seleção/spawn da 18.A. (Concluído em 2026-07-03)
### Fase 3 — HTTP e testes
- [x] Mapear erros em `pipeline.rs` sem cair em 500 genérico. (Concluído em 2026-07-03)
- [x] Testar fila cheia, timeout e cancelamento. (Concluído em 2026-07-03)
- [x] Testar stream longo + segundo processo livre. (Concluído em 2026-07-03)

## Verification

```bash
cargo test -p edger-worker metrics_ephemeral integration_pool cancel_safety
cargo test -p edger-orchestrator pipeline_integration
cargo test --workspace
ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
# Saturar um worker configurado com maxProcesses baixo e queueLimit 0/1.
curl -i http://127.0.0.1:19080/<worker-lento>
curl -s http://127.0.0.1:19080/metrics | rg "queue|rejected|wait"
```

## Status

**completed** (2026-07-03) — Validado live: worker persistente maxProcesses=1/queueLimit=1/queueTimeout=400ms com sleep 1200ms -> req1 200 (serve), req2 503 (queue timeout), req3 429 (queue full). Suite completa verde (49). Footgun bare-manifest registrado em follow-ups/.
