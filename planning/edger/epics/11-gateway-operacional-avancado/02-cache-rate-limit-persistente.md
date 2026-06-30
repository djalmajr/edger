# Story 11.02: Cache e rate limit persistente

**Origin:** `planning/edger/epics/11-gateway-operacional-avancado/00-overview.md`

## Context

Rate limit local em memoria ja existe, mas Buntime entrega valor operacional quando regras podem sobreviver a restart e quando cache reduz custo de upstream. No edger isso deve ficar atras de provider duravel e nao em logica espalhada.

**Depende de:** Story 11.01, Epic 09

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/src/lib.rs` | edit | Adicionar armazenamento opcional de cache e counters |
| `edger-ext-gateway/tests/gateway_middleware.rs` | edit | Provar persistencia e isolamento |
| `edger-orchestrator/tests/state_services.rs` | edit | Provar uso via provider duravel configuravel |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar modo local e limites |

## Detail

### AS-IS

- Rate limit usa buckets locais em memoria.
- Gateway history ja pode usar provider externo em prova isolada.
- Cache nao existe.

### TO-BE

- Cache opcional usa provider duravel com TTL, key derivada segura e invalidacao basica.
- Rate limit persistente/distribuido usa provider apenas quando configurado.
- Sem provider, comportamento local atual continua valido.

### Scope

- **In:** cache local/persistente, counters persistentes opcionais, testes de fallback.
- **Out:** cache distribuido multi-regiao, invalidacao por painel remoto, CDN.

### Critérios de aceite

- [ ] Cache hit/miss e TTL sao observaveis em teste local.
- [ ] Rate limit persistente sobrevive a reconstrução do modulo quando provider estiver configurado.
- [ ] Sem provider, gateway continua usando memoria local.
- [ ] Provider remoto especifico nao aparece em `edger-core`.

## Tasks

- [ ] Definir keys e schema sobre `DurableSqlProvider` ou provider apropriado.
- [ ] Implementar cache com TTL e redaction.
- [ ] Implementar rate limit persistente opcional.
- [ ] Cobrir fallback sem provider e persistencia com provider.

## Verification

```bash
cargo test -p edger-ext-gateway
cargo test -p edger-orchestrator --test state_services
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

