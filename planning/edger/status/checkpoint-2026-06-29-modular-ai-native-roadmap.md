# Checkpoint: modular AI-native roadmap reframe

Date: 2026-06-29
Status: planning structure updated, implementation still pending

## Objective

Reframe the Buntime parity work so Epic 08 stays a consolidation/mapping epic and new execution phases are split by module boundary, lifecycle and value surface.

## Decisions captured

- Epic 08 is now closed as value-parity consolidation, not a container for every future feature.
- Turso remote/sync stays in Epic 09 as an opt-in external provider over `DurableSqlProvider`.
- Extension/plugin operation moved to Epic 10.
- Advanced gateway operation moved to Epic 11.
- Frontends, cPanel/admin UI, shell and module catalog moved to Epic 12.
- MCP/AI-native authoring moved to Epic 13 and requires a first functional local implementation, not only roadmap text.
- No remote deploy is included in this phase; validation remains local, with `docker-compose` allowed only for local dependencies.

## Artifacts updated

- `planning/edger/roadmap.md`
- `planning/edger/epics/08-valor-buntime/00-overview.md`
- `planning/edger/docs/value-parity-matrix.md`
- `planning/edger/epics/10-operacao-extensoes-plugins/00-overview.md`
- `planning/edger/epics/11-gateway-operacional-avancado/00-overview.md`
- `planning/edger/epics/12-frontends-modulares-cpanel/00-overview.md`
- `planning/edger/epics/13-mcp-authoring-ai-native/00-overview.md`

## New backlog shape

- Epic 10 has 4 planned stories: inventory, reconcile/reload, manifest/configuration and local extension validation.
- Epic 11 has 4 planned stories: proxy forwarding, persistent cache/rate limit, operational history/SSE and vhosts.
- Epic 12 has 4 planned stories: cPanel/admin UI scope, shell/catalog, frontend packaging and local Browser validation.
- Epic 13 has 5 planned stories: machine-readable contracts, MCP server, local worker authoring, local worker validation and commit/PR preparation.

## Verification

```bash
python3 planning/edger/scripts/refinement-lint.py --scope planning/edger --round modular-ai-native-reframe
SCRATCH=planning/edger/status/evidence planning/edger/scripts/run-gates.sh
```

Latest refinement-lint result before the gate:

- RED: 0
- WARN: 10
- INFO: 82
- VERDICT: PASS

Rust implementation was not changed in this checkpoint. The full Rust gate remains required before claiming any implementation story complete.

