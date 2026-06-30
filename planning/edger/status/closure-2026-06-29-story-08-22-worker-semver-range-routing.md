# Closure: Story 08.22 — Worker semver range routing

Date: 2026-06-29
Story: `planning/edger/epics/08-valor-buntime/22-worker-semver-range-routing.md`
Status: completed

## Files changed

- `edger-orchestrator/src/manifest_index_stub.rs`
- `edger-orchestrator/tests/routing_resolution.rs`
- `planning/edger/docs/compat-matrix.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/epics/08-valor-buntime/22-worker-semver-range-routing.md`
- `planning/edger/roadmap.md`
- `planning/edger/status/checkpoint-2026-06-29-epic-08-value-parity.md`
- `planning/edger/status/evidence/story-08-22-runtime.txt`

## What changed

- `ManifestIndex::resolve_worker` now treats `latest`, exact versions and
  version ranges as distinct cases.
- Exact requests such as `1.0.0` still require that exact version.
- Range requests using `semver::VersionReq`, such as `^1.0.0` and `~1.2.0`,
  choose the highest enabled version that satisfies the range.
- Range requests with no satisfying version return `NOT_FOUND`.
- The value matrix line `Worker addressing, namespace e semver` is now `tested`.

## Verification

- `cargo test -p edger-orchestrator --lib manifest_index_stub::tests`
- `cargo test -p edger-orchestrator --test routing_resolution semver_range`
- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- `cargo fmt -- --check`
- `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`
- `git diff --check`

All verification commands passed on 2026-06-29.

## Remaining gaps

- Per-version worker mutation selection remains out of scope for Story 08.22.
- Upload/install persistence and hot reload remain future operational slices.
