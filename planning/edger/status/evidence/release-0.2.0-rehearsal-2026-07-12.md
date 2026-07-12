# Release 0.2.0 rehearsal — 2026-07-12

## Scope

Local release-boundary rehearsal for the first planned O'Saasy-licensed EdgeR
release. No tag, push, registry publication or deployment was performed.

## Release identity

- Workspace crates: `0.2.0`.
- Helm chart and `appVersion`: `0.2.0`.
- Changelog target: `0.2.0` (`Unreleased`).
- License: O'Saasy 1.0 through the workspace `license-file` contract.

## Passed gates

- `cargo test --workspace`
- `cargo clippy --workspace -- -D warnings`
- `cargo fmt -- --check`
- `cargo test -p edger-orchestrator --features otel`
- `helm lint charts/edger`
- default Helm render
- Helm render with OTLP and an existing root-key Secret
- `planning/edger/scripts/cpanel-ui-gate.sh`
- `SCRATCH=/tmp/edger-release-gates planning/edger/scripts/run-gates.sh`
  - refinement: PASS, zero red flags
  - path preflight: zero missing references
  - deploy layout: PASS
  - minimal-runtime extension boundary: PASS
  - cargo check: PASS

The extension boundary gate was updated because it still required the removed
`registry.rs` and `admin_workers_plugins.rs` surfaces. It now proves the Epic 17
contract: those obsolete in-process extension/gateway surfaces remain absent and
their removal remains explicit in the compatibility matrix.

## Runtime smoke

The active local runtime on `127.0.0.1:19080` returned:

- `/health`: `200`, `{"status":"ok"}`
- `/ready`: `200`, `{"status":"ready"}`
- `/api/admin/session` with the root key: authenticated `root` administrator
- `/cpanel/`: `200`, title `EdgeR cPanel`

## Docker limitation

`docker build --label org.opencontainers.image.version=0.2.0 -t
edger:0.2.0-rc .` was canceled after 261 seconds. BuildKit never entered the
project build stages; it remained resolving metadata for
`docker.io/denoland/deno:debian` and `docker.io/library/rust:1-bookworm`.
Therefore this is recorded as an external registry/network limitation, not as a
validated image or a code regression. Image labels and container smoke remain a
release-day gate when the registry is responsive.

## Publication boundary

The repository is prepared for a `0.2.0` candidate, but creating a tag,
publishing an image or releasing artifacts requires explicit operator approval.
