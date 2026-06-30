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

- Regra de proxy define upstream permitido, metodos, headers repassados, timeout e limites.
- SSRF guard bloqueia localhost, link-local, IP privado ou esquemas nao permitidos quando a politica exigir.
- Logs nao expõem Authorization, cookies, query sensivel ou body.

### Scope

- **In:** proxy local, allowlist, timeout, redaction, testes.
- **Out:** cache, SSE, vhosts, TLS publico, deploy remoto.

### Critérios de aceite

- [ ] Request para upstream permitido retorna status/body esperados.
- [ ] Upstream nao permitido falha com erro operacional claro.
- [ ] SSRF guard bloqueia destinos inseguros conforme politica.
- [ ] Logs e diagnostics nao vazam segredos de request.

## Tasks

- [ ] Definir contrato de regra de proxy e politica de upstream.
- [ ] Implementar forwarding com limites e timeout.
- [ ] Implementar SSRF guard e redaction.
- [ ] Adicionar testes locais com upstream controlado.

## Verification

```bash
cargo test -p edger-ext-gateway
cargo test -p edger-orchestrator --test value_parity
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

