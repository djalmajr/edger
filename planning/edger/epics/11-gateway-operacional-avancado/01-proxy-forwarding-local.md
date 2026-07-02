# Story 11.01: Proxy forwarding local

**Origin:** `planning/edger/epics/11-gateway-operacional-avancado/00-overview.md`

## Context

O gateway atual cobre redirects e rate limit local, mas ainda nao encaminha requests para upstream externo. Essa story implementa proxy local testavel, com seguranca explicita, antes de cache, historico ou vhosts.

**Depende de:** Epic 08.15, Epic 08.18

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/src/lib.rs` | edit | Implementar regra de proxy forwarding com limites |
| `edger-ext-gateway/tests/gateway_middleware.rs` | edit | Provar upstream permitido, bloqueios e redaction |
| `edger-orchestrator/tests/value_parity.rs` | edit | Cobrir fluxo representativo quando fizer sentido |
| `docs/developers/06-operacao-e-testes.adoc` | edit | Documentar configuracao local segura |

## Detail

### AS-IS

- Gateway aplica CORS/preflight, redirect por prefixo e rate limit local.
- Logs e stats existem, mas nao ha upstream HTTP externo.

### TO-BE

- Regra de proxy define upstream loopback permitido, headers repassados, timeout e limites globais herdados do runtime.
- SSRF guard bloqueia esquemas nao permitidos e destinos nao-loopback nesta fatia local.
- Logs nao expõem Authorization, cookies, query sensivel ou body.

### Scope

- **In:** proxy local, allowlist, timeout, redaction, testes.
- **Out:** cache, SSE, vhosts, TLS publico, deploy remoto.

### Critérios de aceite

- [x] Request para upstream permitido retorna status/body esperados.
- [x] Upstream nao permitido falha com erro operacional claro.
- [x] SSRF guard bloqueia destinos inseguros conforme politica local loopback.
- [x] Logs e diagnostics nao vazam segredos de request.

## Tasks

- [x] Definir contrato de regra de proxy e politica de upstream.
- [x] Implementar forwarding com limites e timeout.
- [x] Implementar SSRF guard e redaction.
- [x] Adicionar testes locais com upstream controlado.

## Verification

```bash
cargo test -p edger-ext-gateway
cargo test -p edger-orchestrator --test value_parity
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

## Status

completed (2026-06-30) - `GatewayProxyRule::try_new` aceita apenas `http://localhost` e `http://127.0.0.1`, preserva suffix/query, aplica timeout local, remove headers sensiveis antes do upstream, retorna `502` operacional sem vazar erro bruto e expõe diagnostics `proxied`/`proxyErrors`. `edger-ext-gateway/tests/gateway_middleware.rs` cobre upstream loopback controlado e rejeicao de destino nao local. Proxy externo amplo, cache, vhosts, SSE e mutacoes dinamicas continuam no Epic 11.
