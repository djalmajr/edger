# Story 02.02: Core data models (Manifest, Config, Principal, ExecutionKind)

**Origin:** `planning/edger/epics/02-edger-core/00-overview.md`  
**Status:** completed (2026-06-29)

## Context
- **Problema:** Only subset WorkerManifest exists; no WorkerConfig parser, Principal, or full Buntime mapping.
- **Objetivo:** Full serde models + `parse_worker_config` + namespace helpers.
- **Valor:** Single source of truth for manifest-driven behavior.
- **Restrições:** Pure functions only; duration/size parsing per Buntime semantics.

## Traceability
- **Source docs:** `planning/edger/design.md` (WorkerManifest mapping table)
- **Depende de:** Story 02.01

## Files
- crates/edger-core/src/manifest.rs
- crates/edger-core/src/config.rs
- crates/edger-core/src/principal.rs
- crates/edger-core/src/execution.rs (ExecutionKind)
- crates/edger-core/src/worker_ref.rs
- crates/edger-core/tests/models_mapping.rs
- crates/edger-core/tests/fixtures/sample_manifest.yaml

## Detail

### AS-IS
Minimal WorkerManifest { name, entrypoint, ttl }.

### TO-BE
Models based on design + Buntime contracts with table-driven tests.

### Acceptance criteria
- [x] WorkerManifest deserializes from YAML fixture matching Buntime sample
- [x] `parse_worker_config` normalizes ttl_ms (0 = ephemeral), sizes, durations
- [x] `WorkerRef` includes namespace + semver fields
- [x] `infer_execution_kind` matches design inference rules
- [x] Table-driven tests for each Buntime field in mapping table

### Dependencies
- Story 02.01

### Pendências
- **RoutesTable inference:** regra 3 do design (export `routes`) requer análise de módulo no loader — adiado para orchestrator/isolation (Epic 05/03); core infere apenas via `kind` explícito ou default `FetchHandler`.
- **WorkerConfig serde:** struct normalizada é runtime-only (`PartialEq`); serde derive adiado até necessidade IPC.

## Test-first plan
- Red: test deserialize manifest fails
- Green: struct + test
- Refactor: separate modules

## Tasks
- [x] Define structs/enums with derives
- [x] Unit tests for roundtrip serde json
- [x] cargo test green

## Verification
```bash
cargo test -p edger-core models
cargo clippy -p edger-core -- -D warnings
! rg -l 'std::fs|tokio::fs|reqwest' crates/edger-core/src/ 2>/dev/null || true
```