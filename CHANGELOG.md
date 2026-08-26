# Changelog

All notable changes to EdgeR will be documented here.

## [0.2.1] - 2026-08-25

### Added

- The dispatch honors `X-Forwarded-Prefix` (charset-checked — the value lands
  inside served HTML) when composing `base_href` and `x-base`, so workers
  behind a stripping proxy (e.g. a Kong route with `strip_path`) emit a
  `<base href>` aligned with the public URL.

### Fixed

- Cross-builds from arm64 hosts: the frontend stage runs on the build
  platform (static output; the x86_64 bun requires AVX2 and dies under
  emulation) and `Dockerfile.cross` cross-compiles the orchestrator with the
  cross GCC as linker instead of emulating rustc, which segfaults under
  Rosetta.
- The labdev chart overlay routes public workers through Kong on the shared
  wildcard host (`/p-` as a string prefix, no strip): the rke2 ingress-nginx
  normalizes every pathType to segment semantics, so a string prefix never
  matches there; the Admin API has no public route at all. The image now
  comes from the public ghcr (SemVer tag, digest-pinned), like tenancit.

## [0.2.0] - 2026-08-25

### Changed

- Future releases adopt the O'Saasy License and are classified as source
  available. Copies previously received under MIT retain the rights granted by
  those distributions.
- The Community/commercial boundary is documented without reintroducing a
  generic plugin runtime.

### Added

- Local-first worker observability in the cPanel: bounded operational events,
  logs, live tail, passive health, request correlation and process lifecycle.
- Optional OTLP traces/logs export with W3C context propagation and
  Helm/Rancher configuration, without making a Collector a runtime dependency.
- Version-scoped worker workspace for files, observability and logs.

### Security

- Upgraded Wasmtime and WASI to a patched release line after the public
  dependency audit identified advisories affecting the previous runtime.
- Bounded and redacted worker console capture.
- Manual/on-deploy health checks without periodic polling that would keep
  serverless workers warm.
