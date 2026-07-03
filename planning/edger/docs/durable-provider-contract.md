# Durable Provider Contract

> OBSOLETE since Epic 17: `DurableSqlProvider`, durable state providers and
> service bindings were removed from the runtime by Story 17.C. This document is
> retained as historical planning context only. See
> `planning/edger/epics/17-edger-minimalista/`.

**Origin:** `planning/edger/epics/09-providers-duraveis-externos/01-contrato-provider-sql-remoto.md`

## Purpose

This document defines the planning boundary for durable SQL providers used by
edger state services. The runtime owns the `DurableSqlProvider` contract in
`edger-core`; local SQLite, Turso remote/sync, or another durable backend are
replaceable implementations registered by composition.

## Boundary

- `edger-core` remains pure vocabulary and must not depend on Turso/libSQL SDKs.
- `edger-orchestrator` selects providers through registration/configuration,
  not through pipeline-specific storage branches.
- Workers, KV/queue and gateway features consume bindings and provider traits;
  they must not learn provider transport details.
- Remote URLs, tokens and sync configuration belong to deployment/runtime
  configuration, not worker manifests.

## Provider tiers

| Tier | Role | Status |
|---|---|---|
| Local SQLite | Single-node demos, tests and local operation | implemented by `edger-ext-turso::LocalSqliteProvider`; `LocalTursoProvider` is a legacy alias |
| External remote/sync | Shared durable state across processes/pods | `edger-ext-turso-remote` exists as a separate provider crate and can be selected by `EDGER_DURABLE_SQL_PROVIDER`; real Turso service evidence remains opt-in |
| Consumer services | KV, queue and gateway history using SQL provider | worker, KV/queue and gateway history have always-on tests against the external provider; consumers remain backend-agnostic |

## Required behavior for remote/sync providers

- Implement the existing `DurableSqlProvider` methods: `execute`, `query` and
  `execute_batch`.
- Preserve namespace isolation in every SQL operation.
- Return operational errors for auth failure, unavailable backend, timeout and
  invalid SQL without exposing secrets.
- Keep credentials out of logs, diagnostics, `x-edger-bindings` and admin API
  responses.
- Provide health/readiness evidence suitable for deployment checks.
- Separate always-on local tests from opt-in tests that require a real remote
  Turso/libSQL target.

## Acceptance hooks for future stories

- Story 09.03 must prove a remote/sync provider against the same trait without
  changing worker manifests.
- Story 09.04 must prove provider selection in the composition root.
- Story 09.05 proved at least one worker, KV/queue flow and gateway durable
  consumer against the external provider.

## Remote Turso/libSQL provider status

`edger-ext-turso-remote` adapts `libsql` remote and remote-replica modes to
`DurableSqlProvider`. Its always-on tests use a configured libSQL local database
to prove SQL contract, namespace isolation and secret-safe diagnostics without
requiring credentials. Real Turso remote/sync evidence is opt-in via
`EDGER_TURSO_TEST_URL`, `EDGER_TURSO_TEST_AUTH_TOKEN` and, for replica sync,
`EDGER_TURSO_TEST_LOCAL_PATH`. Runtime selection uses
`EDGER_DURABLE_SQL_PROVIDER=local|turso-remote|turso-sync` in the `edger`
composition root.

## Consumer evidence

Story 09.05 keeps the Buntime responsibility boundary: the provider owns
connection/sync transport, while each consumer owns its schema and behavior.
Always-on tests cover:

- `edger-orchestrator/tests/state_services.rs`: a worker with `durableSql`,
  `keyValue` and `queue` bindings receives the same binding descriptors when the
  registry uses `RemoteTursoProvider`.
- `edger-ext-keyval/tests/external_provider_contract.rs`: `SqlKeyValueProvider`
  preserves `set/get/delete` and `enqueue/dequeue/ack` over the external
  provider.
- `edger-ext-gateway/tests/gateway_middleware.rs`: `GatewayExtension` persists
  `gateway_decisions` through `with_history_store` and diagnostics expose the
  persistent count without serializing request body or auth headers.

## Non-goals

- This contract does not require renaming the existing local provider crate.
- This contract does not require changing `DurableSqlProvider` before evidence
  shows the current synchronous shape is insufficient.
- This contract does not define a public marketplace or packaging format.
