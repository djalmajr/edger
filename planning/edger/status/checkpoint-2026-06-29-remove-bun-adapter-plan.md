# Checkpoint — remoção do adapter Bun e plano funcional

**Data:** 2026-06-29

## Decisão

`edger.ts` e `edger.test.ts` foram removidos. O runtime ativo passa a ser exclusivamente o binário Rust:

```bash
ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger
```

## Motivo

O adapter Bun foi útil apenas como bootstrap histórico. Ele confundia o objetivo atual porque fazia os exemplos JS/TS parecerem funcionais fora do caminho Rust, enquanto o orquestrador real ainda usa `MockIsolate` para JS/TS.

## Plano

O plano ativo para chegar a uma versão funcional está em:

- `planning/edger/runtime-functional-plan.md`

## Impacto nos gates

- Gate obrigatório continua Rust: `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt -- --check`.
- `bun test` virou gate opcional: só roda se existir suíte JS/TS raiz.
- `run-gates.sh` registra skip explícito quando não há suíte JS/TS raiz.

## Próximo passo

Executar `planning/edger/epics/07-avancado/04-real-js-execution.md`, começando por boot real de `deno_core`.
