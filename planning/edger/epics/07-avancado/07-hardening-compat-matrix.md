# Story 07.07: Hardening, limites e matriz de compatibilidade Buntime

**Origin:** `planning/edger/epics/07-avancado/00-overview.md`

## Context
- **Problema:** Foundation phase fecha sem enforcement rigoroso de limites body/header, sem testes sistemáticos de paridade Buntime, e sem baselines de performance documentadas (decisão usuário: measurement em PR 12).
- **Objetivo:** Aplicar hardening de segurança (limites, sanitização, erros de domínio), implementar matriz de compatibilidade Buntime automatizada, portar harness de performance e definir targets/baselines.
- **Valor:** Confiança para migração Buntime → edger; critérios objetivos de “foundation complete”; CI bloqueia regressões de contrato.
- **Restrições:** Alinhar HeaderLimits Buntime (100 headers, 64KiB total, 8KiB/value); harness não bloqueia CI lento — tier `#[ignore]` para perf; tree sempre verde sob gate strict.

## Traceability
- **Source docs:** `planning/edger/design.md` (PR 12, Security, Risks Buntime fidelity, Measurement, HeaderLimits)
- **Design PR:** PR 12
- **Buntime refs:** `planning/edger/design.md` (mapping table + Migration notes); recall via ai-memory scope `zommehq/buntime` quando necessário
- **Depende de:** 07.02, 07.03, 07.06 (features completas para compat E2E)

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-orchestrator/src/limits.rs` | create | Body size cap, header limits no ingress |
| `edger-core/src/errors.rs` | edit | Erros client-visible (`PayloadTooLarge`, `HeaderLimitExceeded`) |
| `edger-orchestrator/src/pipeline.rs` | edit | Aplicar limits antes de hooks/worker |
| `edger-orchestrator/tests/limits_test.rs` | create | Rejeita body/header acima do cap |
| `edger-orchestrator/tests/compat/mod.rs` | create | Suite matriz Buntime (integration no orchestrator) |
| `edger-orchestrator/tests/compat/manifest_fields.rs` | create | Cada campo do mapping table |
| `edger-orchestrator/tests/compat/auth_namespace.rs` | create | Root, namespaced, publicRoutes |
| `edger-orchestrator/tests/compat/worker_lifecycle.rs` | create | TTL sliding, ephemeral, maxRequests |
| `edger-orchestrator/tests/compat/routing.rs` | create | `/@scope/name@ver`, reserved paths |
| `edger-orchestrator/tests/compat/shell_spa.rs` | create | inject_base, asset paths |
| `edger-orchestrator/benches/` ou `edger-orchestrator/tests/perf/harness.rs` | create | Harness portado Buntime (spawn, p95) |
| `planning/edger/docs/compat-matrix.md` | create | Tabela comportamento Buntime ↔ edger + tiers |
| `planning/edger/docs/performance-baselines.md` | create | Targets e resultados iniciais PR 12 |
| `.github/workflows/ci.yml` | criar (nesta story) | Job compat + optional perf bench |
| `CONTRIBUTING.md` ou `AGENTS.md` | edit | Gate discipline, never publish crates.io |

## Detail

### AS-IS
- Limites documentados no design mas não enforced uniformemente.
- Testes unitários por crate; sem suite `tests/compat` cross-cutting.
- Sem harness de perf nem baselines publicadas.
- Erros podem vazar `anyhow` strings internas.

### TO-BE
- Ingress: `max_body_size` global + per-worker override do manifest; headers contados e somados com limites Buntime.
- Erros mapeados para status HTTP corretos (413, 431, 401, 403, 404, 502, 504).
- Matriz compat: um teste (ou submódulo) por linha crítica do mapping table + behaviors listados em Rollout Migration notes.
- Harness perf mede: cold spawn, warm hit, p95 request (mock worker leve + real JS fixture), pool hit rate sob N requests.
- `planning/edger/docs/compat-matrix.md` marca cada item: ✅ tested | ⚠️ partial | ❌ gap com rationale.
- `planning/edger/docs/performance-baselines.md` registra números iniciais e targets aspiracionais (<50ms spawn cached per design).
- CI: `cargo test --workspace` inclui compat; `cargo test --workspace -- --ignored` para perf local/ nightly.

### Scope
- **In:** Limits, error types, compat test suite, perf harness, docs, CI wiring.
- **Out:** Certificação formal Buntime; load test multi-proc cluster; pen test externo.

### Acceptance criteria
- [ ] Request com body > `max_body_size` retorna 413 sem dispatch ao worker.
- [ ] Request com >100 headers ou header >8KiB rejeitado com erro tipado.
- [ ] Suite `edger-orchestrator/tests/compat/` verde cobrindo ≥90% das linhas “must preserve” do design Migration notes.
- [ ] `planning/edger/docs/compat-matrix.md` publicado com status por comportamento.
- [ ] Harness executa e grava baselines em `planning/edger/docs/performance-baselines.md` (mesmo que targets não atingidos — documentar gap).
- [ ] `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check` verde.
- [ ] Nenhum warning novo em crates touched; regra publish documentada.

### Dependencies
- Story 07.02 (shell compat tests)
- Story 07.03 (cron compat se incluído na matriz)
- Story 07.06 (métricas para perf harness correlation)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`edger-orchestrator/tests/`, `edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Limites e erros
- [ ] `limits.rs` no orchestrator: tower/hyper layers ou checks manuais.
- [ ] `edger-core/errors.rs`: enum público + `IntoResponse` ou mapper no orchestrator.
- [ ] Testes negativos body/header.

### Fase 2 — Matriz compat
- [ ] Scaffold `edger-orchestrator/tests/compat/` com helpers (start test server, fixtures).
- [ ] Implementar módulos: manifest, auth, lifecycle, routing, shell.
- [ ] Gerar `planning/edger/docs/compat-matrix.md` a partir de checklist rastreável.

### Fase 3 — Performance harness
- [ ] Portar scripts/patterns do Buntime (referência em memory scope buntime).
- [ ] Bench spawn latency + p95 com fixture `workers/js-fetch`.
- [ ] Marcar benches pesados `#[ignore]`; documentar invocação.

### Fase 4 — CI + closure
- [ ] CI job compat obrigatório.
- [ ] Atualizar roadmap Fase 7 status; epic acceptance checklist.
- [ ] `/agile-refinement` no épico completo.

## Verification
```bash
cargo test --test compat
cargo test -p edger-orchestrator -- limits
cargo test --workspace -- --ignored
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
bun test
test -f planning/edger/docs/compat-matrix.md
```
