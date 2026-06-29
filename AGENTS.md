# AGENTS.md for edger

**Single canonical rules for edger development.** (root; planning/edger/AGENTS.md mirrors for reference)

## Core
- Core (edger-core or lib) is pure vocabulary: no I/O.
- Always run `bun test` + lints before claiming complete.
- Use colocated .test.ts
- Small behavior preserving changes.
- Preserve worker/extension isolation.
- Update this, roadmap, epics and status when state changes.
- Use explicit memory scopes (workspace: "djalmajr", project: "edger") for ai-memory.
- For buntime cross-ref use zommehq/buntime scope explicitly.

## Launch / Workers
- edger entry: `bun edger.ts --dir <worker-dir> [--port N]`
- Worker dir **must** have `index.{ts,js,mjs}` compatible with:
  - `Deno.serve(handlerOrOptions)`
  - or `export default { fetch(req) {} }`
  - or `export default fetchFn`
- Copy examples verbatim from edge-runtime/examples into workers/<name>/ (preserve index).
- Prefer pure fetch/stream examples for immediate run; document remote/deno.* deps.

## Discipline
- Run `bun test` before report complete. No exceptions.
- Run `memory_lint` (with explicit scope) + `agile-refinement` periodically on planning docs.
- Fix all warnings even in untouched files.
- Rust: cargo test / clippy / fmt when src added.
- No emojis in code/comments/commits.
- Naming: kebab for files, Pascal types, camel funcs.

## Process
- Follow agile flow: intake/roadmap/epic/story/tdd/status/refinement.
- Update docs as progress; lint to prevent staleness.
- Evidence for launches: capture bodies to scratch or logs.
- Fallback to Bun adapter ok per plan risks for embedding; pure logic portable.

## Verification gate
- bun test
- memory_lint (edger scope)
- agile-refinement report clean
- multiple `bun edger.ts --dir workers/xxx` + curl responses match expected
- docs cross-refs current (no stale to non-existing epics/stories)

<!-- ai-memory:start -->
## Long-term memory (ai-memory)

This project uses [ai-memory](https://github.com/akitaonrails/ai-memory)
for cross-session continuity.

**Default to the current project - always.** Every ai-memory tool
auto-scopes to the project resolved from your session's working
directory. **Do NOT pass `project`, `workspace`, or `cwd` arguments unless
the user explicitly references a *different* project by name** (e.g. "what
did we decide in the `other-app` project?"). Phrases like "this project",
"here", "we", "our work", and "where did we leave off" all mean the
*current* project, so call tools with no scoping args.

This default assumes the MCP client can identify the current agent
session. Static MCP clients in parallel sessions for the same user cannot
forward the real agent session id automatically; pass explicit
`workspace` + `project` / `scopes`, or use a session-aware bridge that
forwards the lifecycle-hook session id on MCP calls.

**Lifecycle hooks already capture every prompt and tool call
automatically.** Do not manually write routine notes. Only write durable
memory when the user explicitly asks to remember or annotate something
permanently.

### Use the installed ai-memory Agent Skills

Detailed tool-routing guidance lives in the installed ai-memory Agent
Skills. When a task matches an installed ai-memory Agent Skill, load and
follow that skill before calling ai-memory tools. The skills cover memory
retrieval, handoffs, durable pages, learning maintenance, and routing
install or refresh work.

### When you write a project rule, write it here

If you're about to write a durable project rule ("always X", "never
Y", "all PRs must ..."), write it in the project's canonical agent instruction file.
Many projects use CLAUDE.md for Claude Code and
AGENTS.md for Codex / OpenCode / Cursor / Gemini CLI, but if the project
says one file is canonical, use that file.

### Refreshing this snippet

This block is maintained by ai-memory. Two ways to refresh it with the
latest binary's recommended copy:

- **From the agent** (no terminal needed): ask "refresh the ai-memory
  routing in this project". The agent calls `memory_install_self_routing`,
  picks the right filename for itself (Claude Code -> `CLAUDE.md`; Codex /
  OpenCode / Cursor / Gemini -> `AGENTS.md`), uses its Write / Edit tool
  to replace or append the returned `markered_block` while preserving
  non-ai-memory user content, then writes or updates each returned
  `managed_skills` item under the selected skill root from `target_hints`
  using its `relative_path`.
- **From the CLI**: `ai-memory install-instructions` (defaults to
  `CLAUDE.md`; pass `--target AGENTS.md` for non-Claude agents or projects
  that use `AGENTS.md` as the canonical instruction file).

Both are idempotent: re-runs replace the block bracketed by
`<!-- ai-memory:start -->` / `<!-- ai-memory:end -->` markers without
disturbing the rest of the file.
<!-- ai-memory:end -->
