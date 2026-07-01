# Story 07.07 Closure: Hardening, limites e matriz compat

## Summary

Story 07.07 is complete for the hardening/compat v1 boundary. The repo now has
orchestrator-facing body/header limit tests, a compatibility matrix smoke test,
an opt-in performance harness with a recorded local baseline, and a GitHub
Actions Rust gate with manual perf execution.

## Delivered

- Added `edger-orchestrator/tests/limits_test.rs` for 413/431 ingress behavior
  and no worker dispatch on rejected requests.
- Added `edger-orchestrator/tests/compat_matrix.rs` to keep critical matrix
  rows and known partials mechanically checked.
- Added `edger-orchestrator/tests/perf_harness.rs` as an ignored harness for
  persistent warm-hit p50/p95 and hit rate.
- Added `.github/workflows/ci.yml` with mandatory Rust gate and manual perf
  harness job.
- Updated compatibility, value parity, performance baseline, Epic 07, pendency
  and roadmap docs.

## Evidence

- `cargo test -p edger-orchestrator --test limits_test` passed.
- `cargo test -p edger-orchestrator --test compat_matrix` passed.
- `cargo test -p edger-orchestrator --test perf_harness -- --ignored --nocapture`
  passed.
- `cargo fmt -- --check` passed.
- `cargo clippy --workspace -- -D warnings` passed.
- `cargo test --workspace -- --ignored` passed.
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`
  passed.
- `planning/edger/status/evidence/story-07-07-runtime.txt` records the covered
  behavior and workspace test caveat.

## Follow-up

- Expand perf scenarios to slow, ephemeral and burst workers.
- Add per-worker body override once manifest semantics are finalized.
- 07.04 embedded `deno_core` and 09.03 Turso remote real remain approval-gated.
