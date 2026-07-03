# Follow-up: manifest bare normaliza worker como efêmero

**Origem:** Story 18.B (fila e backpressure por worker), validação de backpressure em `edger-orchestrator` (2026-07-03).

## Problema

Um manifest "bare" (sem `ttl`, `kind` nem `entrypoint`) é normalizado como
efêmero (`ttl_ms == 0`). Se o autor configurar `queueLimit`/`maxProcesses` nesse
manifest esperando backpressure de worker persistente, esses campos são
silenciosamente ignorados: o worker efêmero segue pelo `EphemeralGate`, não pela
fila bounded persistente.

Hoje não há erro nem aviso para essa configuração ambígua.

## Impacto

Baixo e em tempo de configuração, mas confunde. O caso apareceu porque os testes
de backpressure do orchestrator na 18.B precisaram setar `ttl` explicitamente
para que o worker fosse classificado como persistente.

## Opções futuras

- Emitir aviso quando `queueLimit` ou `maxProcesses > 1` aparecerem em worker
  classificado como efêmero.
- Tratar a presença de `queueLimit`/`maxProcesses` como sinal de worker
  persistente na normalização.
- Documentar no manifest reference que backpressure persistente exige worker
  persistente (`ttl > 0` ou `kind`/`entrypoint`).

## Recomendação

Implementar aviso + documentação como mínimo. Isso preserva a normalização atual
e torna o footgun visível sem mudar semântica em uma correção lateral.
