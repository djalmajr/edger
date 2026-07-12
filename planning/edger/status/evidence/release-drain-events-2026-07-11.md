# Evidência 2026-07-11: eventos de release e drain

## Release e migration

- `release.started`, `release.succeeded`, `release.failed` e `release.skipped` usam source `release` no store local.
- Sucesso, falha e marker idempotente são cobertos por testes.
- Falha preserva ausência de `.edger-release` e devolve o erro de deploy; o evento guarda apenas código allowlisted e não comando/stderr.
- Startup e rescan administrativo usam o mesmo produtor de eventos.

## Drain e termination

- TTL, max requests, ephemeral, erro crítico, recycle de stream e shutdown global emitem `process.drain.started`, `process.drain.completed` ou `process.drain.timed_out`, seguidos de `process.terminated`.
- O ACK do harness agora diferencia `timedOut` da contagem de promises registradas; isso evita marcar como completo um `waitUntil` infinito.
- Eventos carregam identidade versionada, process ID quando o backend possui processo, duração, causa e contagem drained.
- Enqueue usa canal bounded e não bloqueante.

## Testes

```text
cargo test -p edger-orchestrator --lib release_tests
5 passed; 0 failed

cargo test -p edger-worker --test integration_pool shutdown_emits_bounded_lifecycle_sequence_per_worker_version
1 passed; 0 failed

cargo test -p edger-isolation --features multiproc --test uds_roundtrip graceful_shutdown_ -- --nocapture
2 passed; 0 failed
```

Os testes incluem release exit 3 com segredo/path no stderr, release success + skip, sequência bounded do pool, promise de 50 ms e promise que nunca resolve.

## Tráfego real e Browser

O fixture `lifecycle-demo@1.0.0` possui `release: true`, TTL de 250 ms, `beforeunload` e um `waitUntil` de 20 ms. Depois de um request HTTP 200, a Admin API mostrou:

1. `release.started` e `release.succeeded` em 6 ms;
2. dispatch real com request ID;
3. `process.drain.started` por `ttl_expired`;
4. console do `beforeunload`;
5. `process.drain.completed`, uma promise drenada em 26 ms;
6. `process.terminated` com process ID `32502-3`.

No Browser, as fontes `Release / migration` e `Process lifecycle` filtraram as sequências pela URL. A tabela mostrou duração, process ID e promise count; refresh preservou a rota e nenhum DOM continha segredo, `file://` ou `/Users/`.
