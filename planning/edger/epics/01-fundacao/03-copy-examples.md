# Story 01.03: Copy edge-runtime examples and verify launches

**Origin:** `planning/edger/epics/01-fundacao/00-overview.md`

## Context
- **Problem:** No worker examples in repo structure; cannot demonstrate edger loader on real edge-runtime patterns.
- **Objective:** Copy 5+ (target 11+) examples verbatim into `workers/` with compatible `index.{ts,js,mjs}` and verify responses.
- **Value:** Repeatable evidence that Bun adapter matches edge-runtime entrypoint contracts.
- **Constraints:** Preserve verbatim indexes; prefer pure fetch/stream examples; document remote/deno.* deps where needed.

## Traceability
- **Source docs:** `planning/edger/design.md` (Buntime fetch contract), `planning/edger/roadmap.md` Fase 1
- **Evidence:** `planning/edger/status/closure-2026-06-28-edger-func.md`

## Files
| Path | Action | Reason |
|---|---|---|
| `workers/<name>/index.{ts,js,mjs}` | create | Verbatim examples from edge-runtime |
| `workers/serve-html/*` | create | Static SPA + readTextFile shim case |
| `planning/edger/status/closure-2026-06-28-edger-func.md` | read | Launch evidence reference |

## Detail

### AS-IS
Loader exists; no workers directory populated.

### TO-BE
11+ worker dirs with index compat; launches return expected bodies for hello, declarative, empty, read-body, chunked, serve-html, stream/sse load.

### Scope
- In: copy + launch verification for pure examples
- Out: full Node compat workers, remote import fixes beyond shims

### Acceptance criteria
- [x] 11+ `workers/` subdirs with `index.{ts,js,mjs}` compatible with Deno.serve or default `{fetch}`
- [x] `bun edger.ts --dir workers/hello-world` returns JSON Hello message
- [x] chunked-text returns `meow`; serve-html serves foo/bar HTML
- [x] stream/sse load without crash

### Dependencies
- Story 01.02 (`02-implement-loader.md`)

## Test-first plan
- **Behavior:** Each copied worker returns documented body via loader
- **Level:** colocated `edger.test.ts` + manual curl launches
- **Avoid:** Testing remote-import workers without shim documentation

## Tasks
- [x] Copy hello-world, serve-declarative-style, empty-response, read-body from edge-runtime/examples
- [x] Copy chunked-text, sse, stream, serve-html, commonjs*, serve
- [x] Verify launches + capture bodies to status/closure evidence
- [x] Extend tests for chunked + serve-html (Deno.readTextFile shim)

## Verification
```bash
bun test
bun edger.ts --dir workers/hello-world --port 8001 &
curl -s -X POST -H 'content-type: application/json' -d '{"name":"Test"}' http://localhost:8001/
bun edger.ts --dir workers/chunked-text --port 8002 &
curl -s http://localhost:8002/
```