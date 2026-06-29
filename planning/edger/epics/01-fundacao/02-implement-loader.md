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

## Test-first plan
- Red: test expects certain body from hello, fails
- Green: impl shim + import
- Refactor: extract pure

## Tasks
- [x] Implement loader with shim
- [x] Add tests for 4+ examples (extended to 6 with chunked + serve-html via TDD)
- [x] Verify launches (multiple examples incl declarative, hello POST, serve-html, commonjs-style exercised)

## Verification
bun test edger.test.ts
real launch + curl for hello (POST), declarative, serve, chunked etc match bodies; evidence in SCRATCH
