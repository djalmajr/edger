# Story 08.10: Env filtering no Deno CLI bridge

**Status:** completed (2026-06-29)

**Origin:** `planning/edger/epics/08-valor-buntime/00-overview.md`

## Context
- **Problema:** A matriz de valor ainda marca `Sensitive env filtering` como `partial`: WASI filtra env sensível, mas o Deno CLI bridge não injeta `manifest.env` e, se usar o ambiente do processo, pode expor variáveis do host.
- **Objetivo:** Injetar apenas env seguro de manifesto no processo Deno, filtrando padrões sensíveis antes do spawn.
- **Valor:** Workers JS/TS recebem configuração explícita sem herdar segredos operacionais do host.
- **Restrições:** `edger-core` continua puro; a sanitização deve reutilizar `is_sensitive_env_key`; `Deno.env` no worker não deve ver segredos como `DATABASE_URL`, `OPENAI_API_KEY`, `_TOKEN` ou `_PASSWORD`.

## Traceability
- **Source docs:** `planning/edger/docs/value-parity-matrix.md`, `planning/edger/docs/compat-matrix.md`, `planning/edger/epics/08-valor-buntime/03-seguranca-e-identidade-operacional.md`
- **Buntime refs:** operational security value: workers must not receive accidental runtime secrets.
- **Prototype refs:** none; this is runtime/security behavior.
- **Business rules:** environment injection is a security boundary and must be deny-by-default for sensitive keys.

## Files

| Path | Action | Reason |
|---|---|---|
| `edger-isolation/src/deno/cli.rs` | edit | Clear inherited environment and inject only filtered manifest env |
| `edger-orchestrator/tests/kind_dispatch_integration.rs` | edit | Add JS/TS observable behavior test for allowed env and blocked secrets |
| `planning/edger/docs/value-parity-matrix.md` | edit | Mark Deno env filtering evidence |
| `planning/edger/docs/compat-matrix.md` | edit | Sync technical compatibility status |
| `planning/edger/status/evidence/story-08-10-runtime.txt` | create | Capture commands and results |

## Detail

### AS-IS
- `WorkerManifest.env` normalizes into `WorkerConfig.env`.
- WASI uses `is_sensitive_env_key` before future env injection.
- Deno CLI bridge spawns `deno eval` without applying manifest env and therefore cannot prove JS/TS env filtering.

### TO-BE
- Deno child process starts with a cleared environment plus a minimal runtime `PATH`.
- Manifest env entries are injected only when their key is not sensitive.
- A JS worker can read a safe `PUBLIC_FLAG` via `Deno.env.get`.
- The same worker cannot read `DATABASE_URL`, `OPENAI_API_KEY`, `GITHUB_TOKEN`, `SERVICE_KEY`, or `ADMIN_PASSWORD`.

### Scope
- **In:** Deno CLI bridge env sanitation, manifest env injection, integration test through the Rust pipeline, matrix/evidence updates.
- **Out:** `.env` file loading, runtime UI for env editing, remote secret stores, env prefix expansion from host process.

### Approach
- Use `Command::env_clear()` before `spawn`.
- Re-add only non-sensitive `WorkerConfig.env` entries plus a minimal inherited `PATH` needed by the Deno executable.
- Keep filtering in Rust before the worker process starts; do not rely only on Deno permission checks because `deno eval` has implicit permissions in the current CLI.

### Risks
- **Breaking Deno execution:** clearing all environment may remove runtime vars Deno expects. Mitigate by retaining `PATH` only and verifying existing Deno integration tests.
- **False security from permissions:** current `deno eval` has implicit permissions, so the real control must be the child environment, not CLI flags.
- **Over-filtering:** public config keys must still reach workers.

### Acceptance criteria
- [x] JS/TS worker receives non-sensitive manifest env.
- [x] JS/TS worker does not receive sensitive manifest env.
- [x] Existing Deno bridge integration tests still pass with cleared environment.
- [x] Value and compatibility matrices no longer claim Deno env injection is missing.

## Test-first plan
- First failing test: a temp JS worker with manifest env returns `PUBLIC_FLAG=visible` and reports sensitive keys as absent.
- Preferred level: orchestrator integration through `kind_dispatch_integration.rs` so the test covers manifest loader, config normalization, worker pool and Deno CLI bridge.
- Low-value tests avoided: direct unit test of `Command` internals or asserting the full environment map.

## Tasks
- [x] Add env filtering/injection helper in `edger-isolation/src/deno/cli.rs`.
  - Done when: child command is built with cleared env and filtered manifest env.
- [x] Add pipeline integration test for Deno manifest env.
  - Done when: test proves safe env is readable and sensitive env is absent from worker behavior.
- [x] Update matrices and evidence.
  - Done when: `Sensitive env filtering` references the Deno evidence.
- [x] Run focused and workspace gates.
  - Done when: Deno integration, workspace Rust gate and planning gate pass.

## Verification
```bash
cargo test -p edger-orchestrator --test kind_dispatch_integration deno_backend_injects_only_filtered_manifest_env
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt -- --check
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```
