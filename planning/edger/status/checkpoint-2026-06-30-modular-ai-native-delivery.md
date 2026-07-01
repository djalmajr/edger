# Checkpoint: Modular AI-native delivery 2026-06-30

## Goal criteria

- Epic 8 remains closed as consolidation/value parity matrix, not as a bucket for new feature work.
- New independent surfaces remain split by ownership:
  - Epic 9: external durable providers, including Turso remote/sync as opt-in provider.
  - Epic 10: extension/plugin operation lifecycle.
  - Epic 11: advanced gateway operation.
  - Epic 12: modular frontends and cPanel.
  - Epic 13: local MCP/AI-native authoring.
- `edger-core` remains vocabulary/contract only; no product UI, provider, gateway proxy, MCP or external integration code moved into core.
- No remote deploy was used as evidence for this phase.

## Delivered in this slice

- `workers/cpanel` adds a minimum useful cPanel/admin UI as a Static SPA worker.
- `workers/shell-demo/manifest.yaml` excludes `cpanel` so `/cpanel` is a standalone frontend module rather than shell content.
- `edger-orchestrator/tests/shell_gateway.rs` proves `/cpanel` bypasses the shell and receives worker base injection.
- `edger-ext-gateway` adds `GatewayProxyRule` for local HTTP loopback proxying.
- `edger-ext-gateway/tests/gateway_middleware.rs` proves loopback forwarding, query/suffix preservation, sensitive header stripping and non-local target rejection.
- cPanel tables escape local inventory values before rendering.
- Browser in-app validated cPanel login, overview, worker/module/gateway/key views and create/revoke of a discard key.
- MCP stdio validation exercised discovery, capability listing, cPanel inspection, dry-run worker authoring and commit/PR preparation with no remote side effect.

## Reclassified or owned gaps

- `must partial` rows in the parity matrix all have explicit owners and evidence:
  - APIs de plugins/extensoes: Epic 10.
  - Durable SQL provider: Epic 9; Turso remote/sync remains opt-in external provider.
  - Gateway/proxy rules: Epic 11.
- Kval/Keyval is mapped to `edger-ext-keyval` as `provider:keyValue`/`provider:queue`, not middleware. Existing local/provider tests remain the evidence; OCC depth and SDK worker surface stay future evolution.
- Gateway now has a functional local proxy slice. Cache, persistent/distributed rate limit, SSE/history operation surface, vhosts and dynamic mutations remain Epic 11 stories.
- cPanel/admin UI has a minimum useful frontend. Shell/catalog derived from `MenuContribution` remains Story 12.02.
- MCP/AI-native local authoring is functional in Epic 13 and was revalidated through stdio against this workspace.

## Evidence

- Browser:
  - `planning/edger/status/evidence/cpanel-browser-2026-06-30.md`
  - `planning/edger/status/evidence/cpanel-browser-2026-06-30.png`
- MCP:
  - `planning/edger/status/evidence/mcp-stdio-2026-06-30.md`
- Tests:
  - `cargo test --workspace`
  - `cargo clippy --workspace -- -D warnings`
  - `cargo fmt -- --check`
  - `SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh`

## Docker/local dependency note

No docker-compose dependency was required for this slice. Gateway proxy validation uses an in-process local loopback upstream in Rust tests. cPanel and MCP validation use only the local edger runtime and stdio server.

## Remaining dedicated work

- Epic 10: reload/rescan, reconcile, module manifest and extension validation lifecycle.
- Epic 11: cache, persistent/distributed rate limit, gateway SSE/history surface, vhosts and dynamic config mutations.
- Epic 12: shell/catalog generated from typed menu/capability contributions.
- Future frontend epics: WebIDE/platform surfaces if they become independently valuable.
