# Public readiness — 2026-07-12

## Scope

Preparation of the EdgeR repository for public visibility without changing the
GitHub repository visibility, publishing an image, or creating a release.

## Public surface prepared

- Public-facing `README.md` aligned with the current persistent Deno process,
  Wasm, cPanel, observability, Helm/Rancher, and optional OTLP capabilities.
- O'Saasy source-available licensing boundary documented in `LICENSE`,
  `CONTRIBUTING.md`, and `README.md`.
- `SECURITY.md`, `CODE_OF_CONDUCT.md`, contribution guidance, issue forms, pull
  request template, Dependabot, and CI workflow added.
- Workspace package metadata now declares repository links, descriptions,
  `rust-version = "1.88"`, and `publish = false`.
- Personal absolute filesystem paths were replaced in tracked documentation and
  evidence. Test-only synthetic paths remain intentionally covered by redaction
  tests.

## Security and dependency evidence

- `gitleaks git . --no-banner --redact`: PASS, 122 commits scanned, no leaks.
- `gitleaks dir` over the exact tracked and non-ignored publication set: PASS,
  6.25 MB scanned, no leaks. The public bearer-token fixture in Story 17.02 is
  explicitly marked `gitleaks:allow` and contains no credential.
- `cargo deny check advisories licenses`: PASS.
- Codex Security scan `6ae300d7-2ff3-4927-9484-ac38696a81ad`: COMPLETE,
  24/24 coverage rows closed and eight findings remediated (one critical, five
  medium, two low). Focused regression tests and the canonical workspace gate
  cover incomplete OIDC configuration, stale JWKS keys, Deno cross-worker
  imports, public stack traces, Helm authentication defaults, Wasm memory/table
  cardinality, and legacy bridge body limits.
- Wasmtime and WASI were upgraded from 29.0.1 to 36.0.12 after the advisory
  audit identified sandbox, out-of-bounds, and WASI capability issues in the
  previous line. Default Wasmtime features were reduced to the features EdgeR
  actually uses.
- `lru` was upgraded to the fixed 0.16 line and the yanked `num-bigint` lockfile
  entry was replaced.
- Two reviewed advisory exceptions are recorded in `deny.toml`:
  - `RUSTSEC-2023-0089`: unmaintained transitive `atomic-polyfill`, no patched
    release and no known vulnerability;
  - `RUSTSEC-2023-0071`: `rsa` is a dev-only dependency used to create ephemeral
    local OIDC test keys, not for EdgeR runtime signing.
- `saffron 0.1.0` has a bundled BSD-3-Clause license file but no SPDX `license`
  manifest field; cargo-deny reports this as a non-blocking warning.

## Verification

- `cargo test --workspace`: PASS.
- `cargo clippy --workspace -- -D warnings`: PASS.
- `cargo fmt -- --check`: PASS.
- `cargo +1.88.0 check --workspace --locked`: PASS.
- `cargo test -p edger-orchestrator --features otel`: PASS.
- `planning/edger/scripts/cpanel-ui-gate.sh`: PASS.
- `/agile-refinement` Mode 1 and `refinement-lint.py`: PASS, zero red flags.
- `planning/edger/scripts/run-gates.sh`: PASS.
- `helm lint charts/edger`: PASS.
- Default and OTLP/existing-Secret Helm renders: PASS.
- `.github/workflows/ci.yml` YAML parse: PASS.
- Final rerun after security remediation: canonical Rust gate, MSRV check,
  optional OTLP test suite, cPanel UI gate, all planning gates, Helm lint and
  default/existing-secret/OTLP renders: PASS.

## External verification limitation

The local Docker daemon was available, but two `docker build` attempts remained
blocked while resolving Docker Hub metadata for `rust:1.88-bookworm` and
`denoland/deno:debian`. The latest attempt was canceled after authentication
succeeded but metadata resolution made no progress; no Dockerfile step ran. CI
contains the same no-push build and must be green before visibility is changed.

## Explicit gates before making GitHub public

1. Review the full dirty worktree and create intentional commits; no public
   readiness changes are committed or pushed by this evidence run.
2. Push and require a green CI run, including the container build.
3. Enable GitHub private vulnerability reporting and branch protection for
   `main`.
4. Confirm that publishing existing Git author metadata, including maintainer
   email addresses already present in history, is acceptable.
5. Confirm the public repository description and topics.
6. Change visibility only after explicit maintainer approval. Creating the
   `v0.2.0` tag and publishing images remain separate release actions.
