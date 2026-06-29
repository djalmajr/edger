# Story 02.02: Core data models (Manifest, Config, Principal, ExecutionKind)

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`

## Context
- **Problema:** Only subset WorkerManifest exists; no WorkerConfig parser, Principal, or full Buntime mapping.
- **Objetivo:** Full serde models + `parse_worker_config` + namespace helpers.
- **Valor:** Single source of truth for manifest-driven behavior.
- **Restrições:** Pure functions only; duration/size parsing per Buntime semantics.

## Traceability
- **Source docs:** `planning/edger/design.md` (WorkerManifest mapping table)
- **Depende de:** Story 02.01

## Files
- edger-core/src/manifest.rs
- edger-core/src/config.rs
- edger-core/src/principal.rs
- edger-core/src/execution.rs (ExecutionKind)
- edger-core/src/lib.rs (reexports + tests)
- edger-core/Cargo.toml (add serde if not)

## Detail

### AS-IS
Minimal WorkerManifest { name, entrypoint, ttl }.

### TO-BE
Models based on design + Buntime contracts:
- WorkerManifest { name, entrypoint, ttl, ... }
- ExecutionKind enum
- ApiKeyPrincipal / namespaces
- Configs
Serde derive, Clone Debug etc. No side effects.

### Acceptance criteria
- [ ] WorkerManifest deserializes from YAML fixture matching Buntime sample
- [ ] `parse_worker_config` normalizes ttl_ms (0 = ephemeral), sizes, durations
- [ ] `WorkerRef` includes namespace + semver fields
- [ ] `infer_execution_kind` matches design inference rules
- [ ] Table-driven tests for each Buntime field in mapping table

### Dependencies
- Story 02.01

## Test-first plan
- Red: test deserialize manifest fails
- Green: struct + test
- Refactor: separate modules

## Tasks
- [ ] Define structs/enums with derives
- [ ] Unit tests for roundtrip serde json
- [ ] cargo test green

## Verification
```bash
cargo test -p edger-core models
cargo clippy -p edger-core -- -D warnings
! rg -l 'std::fs|tokio::fs|reqwest' edger-core/src/models.rs 2>/dev/null || true
```
