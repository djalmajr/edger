# Closure — MVP funcional do runtime + fechamento 07.01/07.02 (2026-07-02)

**Objetivo da sessão:** prosseguir o desenvolvimento até uma versão funcional
do edger (critério: MVP funcional do `runtime-functional-plan.md`).

## Resultado

**MVP funcional entregue e validado ao vivo.** Todos os critérios do plano
passam pelo caminho Rust (`cargo run -p edger-orchestrator --bin edger` +
curls autenticados); evidência em
`status/evidence/runtime-functional-mvp-2026-07-02.txt`.

## Files Changed

- `edger-worker/src/pool.rs` — bugfix: erro de isolate recicla o worker
  (CriticalError + eviction) em vez de deixá-lo preso em `Active` (todo
  request seguinte falhava com `worker not ready for dispatch`).
- `edger-worker/tests/pool_error_recovery.rs` — regressão do bugfix (mutação
  provada: red sem o fix).
- `edger-isolation/src/deno/cli.rs` — sandbox: `deno eval` (permissão total) →
  `deno run --no-prompt` com script de bridge efêmero no worker dir,
  `--allow-read=<worker_dir>`, `--allow-env` sobre env limpo/filtrado,
  `--allow-net` configurável (`EDGER_DENO_ALLOW_NET`); dispatch real de
  `routes` export (exact > `:param` > `*`, method map 405, fallback fetch,
  404 sem fallback).
- `edger-isolation/tests/deno_sandbox.rs` — sandbox: leitura fora do worker
  dir negada, escrita negada, leitura própria permitida.
- `edger-isolation/Cargo.toml` — `tempfile` como dependência regular.
- `edger-core/src/config.rs` — bugfix: `injectBase: false` era ignorado com
  `kind: spa` explícito (`infer_execution_kind` fixava `inject_base: true`).
- `edger-orchestrator/tests/shell_routing_test.rs` — novo: SPA namespaced
  `/@team/panel` com base injetado, asset relativo, `injectBase: false`.
- `edger-orchestrator/tests/kind_dispatch_integration.rs` — novos E2E:
  RoutesTable (2 testes) e Fullstack 501.
- `workers/routes-demo/` — fixture do contrato `routes`.
- Planning/docs: stories 07.01 e 07.02 → completed; epic 07 overview →
  functional-complete; `docs/pendencies-epic-07.md`, `docs/compat-matrix.md`
  (`routes` → tested), `docs/shell-protocol.md` (Evolução planejada),
  `roadmap.md`, `runtime-functional-plan.md`, `AGENTS.md`,
  `docs/adr/0004`, `docs/business/02-glossario.adoc`.

## Plan Status

- [x] Baseline + MVP live validado (incluindo bugfix de pool descoberto no boot)
- [x] Harden da bridge Deno CLI (sandbox de permissões)
- [x] `execute_routes` real (RoutesTable)
- [x] Story 07.02 shell routing fechada
- [x] Story 07.01 manifests+kinds fechada (E2E por todos os ExecutionKind)
- [x] Gates + evidência + planning atualizado

## Breaking Changes

- Workers deixam de ter permissão total no host: sem write/run/ffi/sys, leitura
  restrita ao próprio diretório. Workers que dependiam de acesso amplo precisam
  de política explícita (`EDGER_DENO_ALLOW_NET` para rede; read fora do worker
  dir não é suportado).

## Tests

- `cargo test --workspace`: 65 suítes, 329 passed, 0 failed (2026-07-02).
- Novos: 3 sandbox (`deno_sandbox.rs`), 2 SPA/base (`shell_routing_test.rs`),
  3 kinds (routes x2 + fullstack 501), 1 regressão de pool.
- `cargo clippy --workspace -- -D warnings`: ok. `cargo fmt -- --check`: ok.

## Gates

- Rust gate: verde (acima).
- Planning gate: `SCRATCH=planning/edger/status/evidence
  planning/edger/scripts/run-gates.sh` → ALL PLANNING GATES PASS
  (refinement round `functional-complete-2026-07-02`, RED 0).
- `bun test`: skipped (sem suíte JS raiz — esperado pós-remoção do adapter).

## Validação no browser/preview builtin (adendo 2026-07-02)

APIs (fetch com Bearer), SPA (navegação document real com screenshots),
fullstack (501 adapter-required) e shell (app montado + 401 protegido)
validados no preview builtin; ver
`status/evidence/browser-preview-2026-07-02.md`. Artefatos novos:
`.claude/launch.json` (launch multi-root), `workers/fullstack-demo/`
(fixture do contrato 501) e `workers/shell-demo` com `shellExcludes`
atualizado (achado: shell protegido oculta workers públicos não excluídos —
registrado como candidato a refinamento).

## Pendências que seguem em aberto (gated / foundation)

- 07.04: `deno_core` embutido (aguarda aprovação explícita do operador).
- 07.05 follow-ups: host WASI real + ABI request/response em linear memory.
- 07.06 follow-up: exporter OTLP real.
- 07.07 follow-ups: Turso auth/argon2 (carry 06.02), perf scenarios, body
  override por worker.
- 09.03: smoke contra Turso remoto real (opt-in por env, aguarda aprovação).
- Streaming passthrough real para `stream`/`sse` (hoje bounded-first-chunk).

## Next Steps

1. Decidir aprovação do spike Fase 1B (`deno_core` boot) — caminho de produção.
2. Wasm foundation (Fase 6 do plano funcional).
3. Streaming passthrough (foundation).
