# Story 02.02: Core data models (Manifest, Config, Principal, ExecutionKind)

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`

## Context
Implement the pure serializable models that higher layers (and Bun adapter future port) will use.

## Files
- edger-core/src/manifest.rs
- edger-core/src/config.rs
- edger-core/src/principal.rs
- edger-core/src/execution.rs (ExecutionKind)
- edger-core/src/lib.rs (reexports + tests)
- edger-core/Cargo.toml (add serde if not)

## Detail
Models based on design + Buntime contracts:
- WorkerManifest { name, entrypoint, ttl, ... }
- ExecutionKind enum
- ApiKeyPrincipal / namespaces
- Configs
Serde derive, Clone Debug etc. No side effects.

## Test-first plan
- Red: test deserialize manifest fails
- Green: struct + test
- Refactor: separate modules

## Tasks
- [ ] Define structs/enums with derives
- [ ] Unit tests for roundtrip serde json
- [ ] cargo test green

## Verification
- cargo test -p edger-core (models)
- No I/O in these files
- Docs updated
