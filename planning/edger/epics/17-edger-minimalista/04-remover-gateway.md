# Story 17.D: Remover gateway extension → API Gateway externo

**Origin:** `planning/edger/epics/17-edger-minimalista/00-overview.md`

## Context

- **Problema:** `edger-ext-gateway` faz rate limit, cache, redirects, host routing e histórico dentro do edger — exatamente o que um API Gateway externo (Kong/APISIX/Envoy/cloud LB) faz melhor e battle-tested.
- **Objetivo:** deletar o gateway extension; ingress (política de borda) passa a ser responsabilidade de um API Gateway externo na frente do edger.
- **Valor:** enxuga o edger; usa ferramenta certa para borda.
- **Restrições:** manter o roteamento **core** de worker (nome/versão → worker) — isso é do edger, não é "gateway". Endpoints admin de diagnóstico do gateway (`/api/admin/gateway/*`) somem junto.

## Traceability
- `edger-ext-gateway` (deletar); `edger-orchestrator/src/admin_api.rs` (`/api/admin/gateway/*`); `bin/edger.rs` (wiring)

## Files
| Path | Action | Reason |
|---|---|---|
| `edger-ext-gateway/` | delete | Borda vira externa |
| `edger-orchestrator/src/admin_api.rs` | edit | Remover rotas `/api/admin/gateway/*` |
| `edger-orchestrator/src/bin/edger.rs` | edit | Remover `gateway_extension_from_env` e envs `EDGER_GATEWAY_*` |
| `planning/edger/docs/deployment-api-gateway.md` | create | Receita: API GW externo (auth OIDC, rate limit, cache) na frente do edger |

## Detail
### Scope
- **In:** deletar crate + rotas/env do gateway; doc de deployment com API GW externo.
- **Out:** escolher/entregar um API GW específico (é infra do operador; doc dá o padrão).

### Acceptance criteria
- [ ] `edger-ext-gateway` deletado; `/api/admin/gateway/*` e `EDGER_GATEWAY_*` removidos; workspace compila.
- [ ] Roteamento core de worker (nome/versão/semver) intacto (suites de routing verdes).
- [ ] Doc de deployment mostra o padrão "API GW externo → edger".

### Dependencies
- Story 17.C

## Tasks
- [ ] Deletar crate + limpar admin/boot; doc de deployment.
- [ ] Rodar suites de routing (garantir que só a borda saiu, não o roteamento core).

## Verification
```bash
cargo build --workspace
cargo test -p edger-orchestrator --test routing_resolution
```

## Status

**completed** (2026-07-02) — Deletados `edger-ext-gateway` (crate + membership + dev-dep),
o wiring no boot (`gateway_extension_from_env`, envs `EDGER_GATEWAY_*`), as rotas
`/api/admin/gateway/*` + handlers, e **também o `shell_gateway`** (roteamento de
micro-frontend: `resolve_shell_worker`, sniff de `sec-fetch-dest`, casos de path de
segmento único). Ingress (rate limit/cache/redirect/host) → **API Gateway externo**;
composição de front-end → **AppShell externo** (ver `follow-ups/appshell-*.md`,
`follow-ups/fallback-worker-routing.md`). O `HomepageFallback` (base `"/"`) foi
**mantido** — é roteamento legítimo de worker na raiz, não shell routing.

Testes deletados (features removidas): `gateway_integration`, `shell_gateway`,
`shell_routing_test`, `registry_providers`, `state_services`, `value_parity`,
`admin_workers_plugins` (entrelaçado demais; lacuna de cobertura admin registrada em
`follow-ups/admin-endpoint-test-coverage.md`). compat-matrix: linhas de ingress/shell
marcadas `removido → externo`. Validado ao vivo: `/ssr-demo` serve o worker direto
(sem shell), control plane segue protegido por root-key. Verde: workspace + multiproc
+ clippy + fmt + E2E deno.

`shellExcludes`/`visibility` ficam vestigiais (limpeza no 17.E).
