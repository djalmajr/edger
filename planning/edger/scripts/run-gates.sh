#!/usr/bin/env bash
# Exit-code driven planning gates. Writes ONLY to $SCRATCH — never edits status docs.
# Usage: SCRATCH=/path/to/scratch ./planning/edger/scripts/run-gates.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
SCRATCH="${SCRATCH:?SCRATCH must be set to the implementer scratch directory}"
mkdir -p "$SCRATCH"

cd "$REPO_ROOT"
SERVE_PID=""
SERVE_PORT=""
LOCAL_CONFIG="$SCRATCH/ai-memory-local.toml"

cleanup() {
  if [[ -n "$SERVE_PID" ]] && kill -0 "$SERVE_PID" 2>/dev/null; then
    kill "$SERVE_PID" 2>/dev/null || true
    wait "$SERVE_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

log() { echo "[run-gates] $*" | tee -a "$SCRATCH/run-gates.log"; }

rm -f "$SCRATCH/gates.ok"
: >"$SCRATCH/run-gates.log"

log "repo=$REPO_ROOT scratch=$SCRATCH"

# --- memory_lint (local ai-memory serve; NOT remote memory.djalmajr.dev) ---
SERVE_LOG="$SCRATCH/ai-memory-serve.log"
SERVE_PORT="$(python3 -c "import socket; s=socket.socket(); s.bind(('127.0.0.1', 0)); print(s.getsockname()[1]); s.close()")"
INVOCATION_TARGET="127.0.0.1:${SERVE_PORT}"

ai-memory serve --transport http --bind "$INVOCATION_TARGET" >"$SERVE_LOG" 2>&1 &
SERVE_PID=$!

for _ in $(seq 1 50); do
  if curl -sf "http://${INVOCATION_TARGET}/mcp" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"run-gates","version":"1.0"}}}' \
    >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

if ! curl -sf "http://${INVOCATION_TARGET}/mcp" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"run-gates","version":"1.0"}}}' \
  >/dev/null 2>&1; then
  log "FAIL memory_lint: ai-memory serve not ready at $INVOCATION_TARGET"
  exit 1
fi
cat >"$LOCAL_CONFIG" <<EOF
server_url = "http://${INVOCATION_TARGET}"
EOF

# Ensure workspace/project exists (idempotent seed)
curl -sf -X POST "http://${INVOCATION_TARGET}/mcp" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"memory_write_page","arguments":{"workspace":"djalmajr","project":"edger","path":"_meta/gate-seed.md","body":"# Gate seed\n\nIdempotent seed for memory_lint scope.\n"}}}' \
  >>"$SCRATCH/run-gates.log" 2>&1 || true

MCP_RAW="$SCRATCH/memory-lint-mcp-raw.json"
HTTP_CODE=$(curl -sS -o "$MCP_RAW" -w "%{http_code}" -X POST "http://${INVOCATION_TARGET}/mcp" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"memory_lint","arguments":{"workspace":"djalmajr","project":"edger","dry_run":true,"no_llm":true}}}')

{
  echo "tool=ai-memory memory_lint (MCP tools/call)"
  echo "invocation_target=${INVOCATION_TARGET}"
  echo "workspace=djalmajr"
  echo "project=edger"
  echo "remote_memory_djalmajr_dev=NOT_USED (local serve only)"
  echo "http_code=${HTTP_CODE}"
  echo "generated=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "--- mcp raw ---"
  cat "$MCP_RAW"
} >"$SCRATCH/memory-lint.txt"

if [[ "$HTTP_CODE" != "200" ]]; then
  log "FAIL memory_lint: HTTP $HTTP_CODE"
  exit 1
fi

FINDINGS_COUNT=$(python3 -c "
import json, pathlib, sys
raw = pathlib.Path('$MCP_RAW').read_text()
data = json.loads(raw)
text = data['result']['content'][0]['text']
findings = json.loads(text).get('findings', [])
print(len(findings))
if findings:
    sys.exit(1)
" 2>>"$SCRATCH/run-gates.log") || {
  log "FAIL memory_lint: non-empty findings"
  exit 1
}

log "PASS memory_lint findings=0 target=$INVOCATION_TARGET"

# --- refinement-lint.py (NOT /agile-refinement skill output) ---
REFINE_TMP="$SCRATCH/refinement-report.tmp"
{
  echo "tool=refinement-lint.py"
  echo "scope=planning/edger/"
  echo "checklist_source=agile-refinement/SKILL.md Mode 1 (implemented by script, not skill invocation)"
  echo "generated=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "========================================================================"
  python3 planning/edger/scripts/refinement-lint.py --scope planning/edger --round run-gates
} >"$REFINE_TMP" 2>&1

mv "$REFINE_TMP" "$SCRATCH/refinement-report.txt"

if grep -q '\[RED\]' "$SCRATCH/refinement-report.txt"; then
  log "FAIL refinement-lint: [RED] findings present"
  exit 1
fi
if ! grep -q 'VERDICT: PASS' "$SCRATCH/refinement-report.txt"; then
  log "FAIL refinement-lint: no VERDICT PASS"
  exit 1
fi
log "PASS refinement-lint"

# --- path preflight ---
set +e
bash planning/edger/scripts/path-preflight.sh . 2>&1 | tee "$SCRATCH/path-preflight.txt"
PF_EXIT=${PIPESTATUS[0]}
set -e
PREFLIGHT_MISSING=$(grep '^Missing:' "$SCRATCH/path-preflight.txt" | awk '{print $2}')
if [[ "$PF_EXIT" -ne 0 || "${PREFLIGHT_MISSING:-1}" != "0" ]]; then
  log "FAIL path-preflight: exit=$PF_EXIT missing=$PREFLIGHT_MISSING"
  exit 1
fi
log "PASS path-preflight"

# --- bun test ---
set +e
bun test 2>&1 | tee "$SCRATCH/bun-test.txt"
BUN_EXIT=${PIPESTATUS[0]}
set -e
if [[ "$BUN_EXIT" -ne 0 ]] || ! grep -q '0 fail' "$SCRATCH/bun-test.txt"; then
  log "FAIL bun test exit=$BUN_EXIT"
  exit 1
fi
log "PASS bun test"

# --- cargo check ---
set +e
cargo check --workspace 2>&1 | tee "$SCRATCH/cargo-check.txt"
CARGO_EXIT=${PIPESTATUS[0]}
set -e
if [[ "$CARGO_EXIT" -ne 0 ]]; then
  log "FAIL cargo check exit=$CARGO_EXIT"
  exit 1
fi
log "PASS cargo check"

# --- inventory ---
find planning/edger/epics -name '*.md' | sort >"$SCRATCH/epics-tree.txt"
python3 - <<'PY' >"$SCRATCH/epics-inventory.txt"
import pathlib
root = pathlib.Path("planning/edger/epics")
for epic in sorted(root.iterdir()):
    if not epic.is_dir():
        continue
    stories = [f for f in epic.glob("*.md") if f.name != "00-overview.md" and not f.name.endswith("spike.md")]
    print(f"{epic.name}: {len(stories)} stories")
PY

# --- summary marker for render-status-from-gates.sh ---
python3 - <<PY >"$SCRATCH/gates-summary.json"
import json, datetime
print(json.dumps({
  "passed_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
  "memory_lint": {
    "tool": "ai-memory memory_lint (MCP)",
    "invocation_target": "$INVOCATION_TARGET",
    "workspace": "djalmajr",
    "project": "edger",
    "findings_count": 0
  },
  "refinement": {
    "tool": "refinement-lint.py",
    "scope": "planning/edger/",
    "red_count": 0
  },
  "path_preflight": {"missing": 0},
  "bun_test": {"fail": 0}
}, indent=2))
PY

touch "$SCRATCH/gates.ok"
log "ALL GATES PASS — wrote $SCRATCH/gates.ok"
exit 0