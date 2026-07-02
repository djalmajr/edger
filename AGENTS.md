# AGENTS.md for edger

**Single canonical rules for edger development.** (root; planning/edger/AGENTS.md mirrors for reference)

## Core
- Product label is **"EdgeR"** in user-facing surfaces (UI titles, brand text, docs prose). Technical identifiers stay lowercase `edger` (crates, binary, env vars, paths, URLs).
- Core (edger-core or lib) is pure vocabulary: no I/O.
- Always run the Rust gate before claiming complete: `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check`.
- Run `bun test` only if a root JS/TS test suite exists; the historical Bun adapter is removed.
- Small behavior preserving changes.
- Preserve worker/extension isolation.
- Update this, roadmap, epics and status when state changes.
- Use explicit memory scopes (workspace: "djalmajr", project: "edger") for ai-memory.
- For buntime cross-ref use zommehq/buntime scope explicitly.

## Extensions (edger-ext-*)
- Crates `edger-ext-*` depend only on `edger-core` — never on `edger-orchestrator`.
- **choose ONE** mode per crate: Middleware, AuthProvider, or WorkerHandler (not mixed without exclusive Cargo features).
- v1 registration: explicit list in bin `edger` via `collect_extensions(vec![...])` (see `planning/edger/docs/extensions.md`).
- Do not publish extension crates to crates.io manually.

## Launch / Workers
- edger entry: `ROOT_API_KEY=test-root PORT=19080 RUNTIME_WORKER_DIRS=workers cargo run -p edger-orchestrator --bin edger`
- Worker dir **must** have `index.{ts,js,mjs}` compatible with:
  - `Deno.serve(handlerOrOptions)`
  - or `export default { fetch(req) {} }`
  - or `export default fetchFn`
- Copy examples verbatim from edge-runtime/examples into workers/<name>/ (preserve index).
- JS/TS workers execute by default on a **persistent Deno process** per worker over a Unix domain socket (Epic 15): the module is imported once and served across requests (warm p50 ~1.6ms end-to-end, ~25x vs v1). Per-worker heap cap via `--v8-flags=--max-old-space-size` (from `ResourceLimits::from_config`); response bodies read as bounded streams (`EDGER_STREAM_MAX_BYTES`/`EDGER_STREAM_IDLE_MS`) so infinite/SSE streams never hang the process. `deno` on PATH or `EDGER_DENO_BIN`; sandboxed with `deno run --no-prompt` (read limited to worker dir + Deno cache, write/run/ffi denied, `--allow-net`/`--allow-env`/`--allow-sys` for npm compat; network configurable via `EDGER_DENO_ALLOW_NET`).
- **Legacy fallback:** `EDGER_JS_RUNTIME=bridge` forces the v1 per-request CLI bridge (`deno run` per request, bounded-first-chunk streaming). It is retained as an emergency fallback only; the persistent process is the supported path. Embedding `deno_core` was evaluated and rejected in favor of the durable multi-process design; do not reintroduce a Bun adapter.
- Workers may export `routes` (Bun.serve-style: exact > `:param` > `*` wildcard, per-method maps, `fetch` fallback) in addition to `Deno.serve`/default fetch.

## Discipline
- Planning maturity: `/agile-refinement` Mode 1 on `planning/edger/` + `refinement-lint.py` (see `planning/edger/scripts/run-gates.sh`). Only the orchestrator agent calls ai-memory tools; subagents must not.
- `memory_lint` (workspace `djalmajr`, project `edger`): orchestrator only, when the remote server is stable — excluded from planning gates if unstable.
- Fix all warnings even in untouched files.
- Rust gate: `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check`.
- `edger-core` is pure vocabulary (no I/O); `Isolate`/`WorkerHandler` use `async-trait` workspace dep.
- No emojis in code/comments/commits.
- Naming: kebab for files, Pascal types, camel funcs.

## Process
- Follow agile flow: intake/roadmap/epic/story/tdd/status/refinement.
- Update docs as progress; lint to prevent staleness.
- Evidence for launches: capture bodies to scratch or logs.
- Do not use the removed Bun adapter as implementation fallback; unblock Rust isolation instead.

## Verification gate
- Rust gate: `cargo test --workspace && cargo clippy --workspace -- -D warnings && cargo fmt -- --check`
- `/agile-refinement` Mode 1 report clean (`planning/edger/status/evidence/refinement-report.txt`)
- memory_lint (edger scope; orchestrator only; optional when server stable)
- Rust launch evidence through `cargo run -p edger-orchestrator --bin edger` + curl responses match expected
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
