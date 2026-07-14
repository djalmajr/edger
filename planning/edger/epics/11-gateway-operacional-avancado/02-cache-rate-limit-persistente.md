# Story 11.02: Cache e rate limit persistente

**Origin:** `planning/edger/epics/11-gateway-operacional-avancado/00-overview.md`

**Status:** completed

## Context

Rate limit local em memoria ja existe, mas Buntime entrega valor operacional quando regras podem sobreviver a restart e quando cache reduz custo de upstream. No edger isso deve ficar atras de provider duravel e nao em logica espalhada.

**Depende de:** Story 11.01, Epic 09

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/src/lib.rs` | edit | Adicionar armazenamento opcional de cache e counters |
| `edger-ext-gateway/tests/gateway_middleware.rs` | edit | Provar persistencia e isolamento |
| `crates/edger-orchestrator/tests/state_services.rs` | edit | Provar uso via provider duravel configuravel |
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

- [x] Cache hit/miss e TTL sao observaveis em teste local.
- [x] Rate limit persistente sobrevive a reconstrução do modulo quando provider estiver configurado.
- [x] Sem provider, gateway continua usando memoria local.
- [x] Provider remoto especifico nao aparece em `edger-core`.

## Tasks

- [x] Definir keys e schema sobre `DurableSqlProvider` ou provider apropriado.
- [x] Implementar cache com TTL e redaction.
- [x] Implementar rate limit persistente opcional.
- [x] Cobrir fallback sem provider e persistencia com provider.

## Verification

```bash
cargo test -p edger-ext-gateway
cargo test -p edger-orchestrator --test state_services
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Closure

completed (2026-07-01) - `edger-ext-gateway` adicionou
`GatewayCacheConfig`, `with_cache_store` e
`with_persistent_rate_limit_store`, ambos sobre `Arc<dyn DurableSqlProvider>`.
O cache persiste respostas publicas `GET`/`HEAD` status `200` com TTL,
observa hit/miss via `x-edger-cache` e `diagnostics.cache`, e grava somente
hash estavel de metodo/host/URI como chave. O rate limit persistente usa janela
fixa em `gateway_rate_limit_buckets`, persiste somente hash da chave do bucket
e bloqueia apos reconstruir o modulo com o mesmo provider; sem provider, o
token bucket em memoria continua local ao modulo. O binario `edger` liga cache,
historico e rate limit persistente por env usando o provider duravel ja
selecionado no composition root. Evidencia local: novos testes direcionados de
cache TTL/hit/miss/redaction e rate limit persistente passaram; o suite completo
`cargo test -p edger-ext-gateway` continua bloqueado no sandbox pelo teste
preexistente de TCP loopback
`proxy_rule_forwards_to_local_upstream_without_sensitive_headers` com
`PermissionDenied`.
