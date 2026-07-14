# Story 01.02: Implement loadWorkerHandler + edger cli

**Origin:** `planning/edger/epics/01-fundacao/00-overview.md`

## Context
Implement the adapter and cli so examples with correct index run and return expected.

## Traceability
- **Source docs:** `planning/edger/design.md` (fetch contract), edge-runtime examples
- **Output:** `edger.ts`, `edger.test.ts`

## Files
- edger.ts
- edger.test.ts

## Detail
- Export loadWorkerHandler
- Cli entry if main
- Support both Deno.serve capture and default export

### Acceptance criteria
- [x] `bun test` passes with 6+ example workers covered
- [x] `bun edger.ts --dir workers/<name>` launches and returns expected bodies
- [x] Loader supports `Deno.serve`, `export default { fetch }`, and default export function

## Test-first plan
- Red: test expects certain body from hello, fails
- Green: impl shim + import
- Refactor: extract pure

## Tasks
- [x] Implement loader with shim
- [x] Add tests for 4+ examples (extended to 6 with chunked + serve-html via TDD)
- [x] Verify launches (multiple examples incl declarative, hello POST, serve-html, commonjs-style exercised)

## Verification
```bash
bun test edger.test.ts
bun edger.ts --dir workers/examples/hello-world --port 19001 &
sleep 1
curl -s -X POST http://127.0.0.1:19001/ -d '{}' | jq .
kill %1 2>/dev/null || true
```
