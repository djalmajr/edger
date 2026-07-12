# Changelog

All notable changes to EdgeR will be documented here.

## [0.2.0] - Unreleased

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
