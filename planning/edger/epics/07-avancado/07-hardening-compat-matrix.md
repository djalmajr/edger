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
| `crates/edger-core/src/wire.rs` | existing | HeaderLimits Buntime port (`100`, `64KiB`, `8KiB/value`) |
| `crates/edger-orchestrator/src/wire.rs` | existing | Body cap + header validation before worker serialization |
| `crates/edger-orchestrator/tests/limits_test.rs` | create | Rejeita body/header acima do cap |
| `crates/edger-orchestrator/tests/compat_matrix.rs` | create | Smoke suite para manter a matriz publicada rastreável |
| `crates/edger-orchestrator/tests/perf_harness.rs` | create | Harness opt-in para warm-hit p50/p95 + hit rate |
| `planning/edger/docs/compat-matrix.md` | edit | Tabela comportamento Buntime <-> edger + tiers |
| `planning/edger/docs/performance-baselines.md` | edit | Targets e resultado inicial PR 12 |
| `.github/workflows/ci.yml` | create | Rust gate obrigatório + perf harness manual |
| `AGENTS.md` | existing | Gate discipline e regra de não publicar crates manualmente já documentadas |

## Detail

### AS-IS
- Limites de headers e body já existiam no wire layer, mas faltava teste
  orchestrator-facing provando status externo e ausência de dispatch ao worker.
- A matriz de compatibilidade já existia e era alimentada por muitos testes
  focados, mas não havia teste mecânico garantindo as linhas críticas/tier.
- Baselines manuais existiam desde Story 08.07; faltava harness opt-in
  reproduzível.
- Não havia workflow CI versionado no repo.

### TO-BE
- Ingress: body cap global e HeaderLimits Buntime aplicados antes do worker.
- Erros mapeados para status HTTP corretos no pipeline, incluindo 413 e 431.
- Matriz compat: linhas críticas publicadas como `tested` e linhas incompletas
  seguem explícitas como `partial`.
- Harness perf mede warm-hit p50/p95 e hit rate para worker persistente
  in-memory; cenários slow/ephemeral/burst ficam para expansão.
- `planning/edger/docs/performance-baselines.md` registra resultado local e
  comando reproduzível.
- CI: Rust gate obrigatório; perf harness roda em `workflow_dispatch`.

### Scope
- **In:** Limits tests, compat matrix smoke suite, opt-in perf harness, docs,
  CI wiring.
- **Out:** Certificação formal Buntime; load test multi-proc cluster; pen test
  externo; per-worker body override; perf scenarios slow/ephemeral/burst.

### Acceptance criteria
- [x] Request com body > `max_body_size` retorna 413 sem dispatch ao worker.
- [x] Request com >100 headers ou header >8KiB rejeitado com erro tipado.
- [x] Suite compat verde cobrindo as linhas críticas publicadas em `compat-matrix.md`.
- [x] `planning/edger/docs/compat-matrix.md` publicado com status por comportamento.
- [x] Harness executa e grava baselines em `planning/edger/docs/performance-baselines.md` (mesmo que targets não atingidos; documentar gap).
- [ ] `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check` verde no ambiente sem sandbox de loopback; local sandbox ainda bloqueia um teste gateway preexistente.
- [x] Nenhum warning novo em crates touched; regra publish documentada.

### Dependencies
- Story 07.02 (shell compat tests)
- Story 07.03 (cron compat se incluído na matriz)
- Story 07.06 (métricas para perf harness correlation)

## Test-first plan
- **Behavior:** Acceptance criteria above fail before implementation; first test targets smallest vertical slice of the story.
- **Level:** crate integration tests (`crates/edger-orchestrator/tests/`, `crates/edger-isolation/tests/`) + workspace gate.
- **Avoid:** Re-implementing production logic inside tests; hard-coded expected values without driving real entry points.

## Tasks

### Fase 1 — Limites e erros
- [x] Reusar wire-layer limits existentes no orchestrator antes de dispatch.
- [x] Mapper do orchestrator retorna 413/431 para `PAYLOAD_TOO_LARGE` e `HEADER_TOO_LARGE`.
- [x] Testes negativos body/header sem dispatch ao worker.

### Fase 2 — Matriz compat
- [x] `compat_matrix.rs` garante linhas críticas e partials conhecidos na matriz publicada.
- [x] Módulos existentes cobrem manifest, auth, lifecycle, routing, shell,
  observability, cron, gateway e Wasm.
- [x] `planning/edger/docs/compat-matrix.md` atualizado a partir de checklist rastreável.

### Fase 3 — Performance harness
- [x] Harness local opt-in mede worker persistente in-memory.
- [x] Bench p50/p95 + hit rate com fixture leve.
- [x] Marcar harness `#[ignore]`; documentar invocação.
- [ ] Cenários slow/ephemeral/burst seguem follow-up.

### Fase 4 — CI + closure
- [x] CI job Rust gate obrigatório e perf harness manual.
- [x] Atualizar roadmap Fase 7 status; epic acceptance checklist.
- [x] `/agile-refinement` no épico completo.

## Verification
```bash
cargo test -p edger-orchestrator --test compat_matrix
cargo test -p edger-orchestrator --test limits_test
cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture
cargo test --workspace -- --ignored
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
test -f planning/edger/docs/compat-matrix.md
```
