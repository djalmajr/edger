# Story 21.12: Health check opcional por worker

**Origin:** `planning/edger/epics/21-observabilidade-workers-cpanel/00-overview.md`

## Context

- **Problem:** alguns apps precisam validar dependências no deploy, mas polling periódico criaria processos e distorceria a semântica serverless.
- **Objective:** permitir check explícito manual ou on-deploy, mantendo o default sem probe.
- **Value:** operador valida uma versão antes de promovê-la sem transformar health em keep-alive.
- **Constraints:** opt-in; GET/HEAD; timeout curto; sem credenciais privilegiadas; sem periodicidade inicial.

## Traceability

- **Prototype:** ação `Run health check` em `OBS-02 Worker detail`; resultado relacionado em `OBS-06 Worker workspace · Logs`.
- **Business rules:** health passivo continua canônico; probe é evidência complementar e não substitui tráfego real.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-core/src/manifest.rs` | edit | Contrato `healthCheck` opcional |
| `edger-core/src/config.rs` | edit | Validar path, method, timeout e modo |
| `edger-orchestrator/src/deploy.rs` | edit | Executar check on-deploy antes de promoção |
| `edger-orchestrator/src/admin_api.rs` | edit | Endpoint root-only de execução manual |
| `workers/cpanel/index.js` | edit | Ação, loading e resultado do check |
| `edger-orchestrator/tests/deploy_admin.rs` | edit | Provar success/failure/timeout e autorização |

## Detail

### AS-IS

- Existem probes do runtime (`/healthz`, `/livez`, `/readyz`), mas não probe por worker.

### TO-BE

- Manifesto opcional:

```yaml
healthCheck:
  path: /health
  mode: manual # manual | on-deploy
  timeout: 2s
```

- `mode` não aceita periodic nesta story.
- Check usa o pipeline normal da versão, request ID próprio e evento operacional sanitizado.
- Falha on-deploy impede promoção/default; falha manual não desabilita automaticamente a versão.

### Scope

- **In:** contrato, check manual/on-deploy, autorização, evento e UI.
- **Out:** polling, auto-healing, Kubernetes probe por worker, dependências externas distribuídas e check mutável.

### Acceptance criteria

- [x] Worker sem `healthCheck` não recebe requests artificiais.
- [x] Check manual não altera default/enabled automaticamente nem contamina health passivo/request total.
- [x] Check on-deploy falho impede promoção, emite motivo sanitizado e remove o candidato do disco/índice.
- [x] Timeout é bounded e não cria polling.
- [x] cPanel diferencia resultado do probe de health passivo.

### Dependencies

- 21.10 e 21.11 concluídas; o caso real é validar dependências antes da promoção sem usar tráfego sintético periódico.

## Tasks

- [x] Validar necessidade após 21.10. **Done:** gate de dependência on-deploy é evidência diferente de health passivo.
- [x] Definir e testar contrato do manifesto. **Done:** modos/timeout/path inválidos falham cedo.
- [x] Implementar execução manual/on-deploy. **Done:** pipeline, auth, timeout e rollback cobertos.
- [x] Emitir evento e renderizar resultado. **Done:** Browser mostra probe separado e evento `health_check` nos logs.
- [x] Validar que não existe polling. **Done:** probe exige ação/config on-deploy e não soma amostras passivas.

## Verification

```bash
cargo test -p edger-core health_check
cargo test -p edger-orchestrator --test admin_endpoints manual_health_check
cargo test -p edger-orchestrator --test deploy_install on_deploy_health_check
planning/edger/scripts/cpanel-ui-gate.sh
cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check
```

## Status

completed (2026-07-12) — contrato, manual/on-deploy, rollback persistente, evento local e UI validados; sem periodicidade.
