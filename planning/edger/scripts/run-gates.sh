#!/usr/bin/env bash
# Planning gates only (/agile-refinement Mode 1 evidence + mechanical checks).
# Writes ONLY to $SCRATCH — never edits status docs.
# Usage: SCRATCH=/path/to/scratch ./planning/edger/scripts/run-gates.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
SCRATCH="${SCRATCH:?SCRATCH must be set}"
mkdir -p "$SCRATCH"

cd "$REPO_ROOT"
rm -f "$SCRATCH/gates.ok"
: >"$SCRATCH/run-gates.log"
log() { echo "[run-gates] $*" | tee -a "$SCRATCH/run-gates.log"; }

# memory_lint intentionally excluded — remote server instability (operator directive 2026-06-29)
log "repo=$REPO_ROOT (planning lint gates only: /agile-refinement + refinement-lint.py)"

# --- /agile-refinement Mode 1 report (agent-produced; must exist before this script) ---
if [[ ! -f "$SCRATCH/refinement-report.txt" ]]; then
  log "FAIL: $SCRATCH/refinement-report.txt missing — run /agile-refinement Mode 1 on planning/edger/ first"
  exit 1
fi
cp "$SCRATCH/refinement-report.txt" "$SCRATCH/agile-refinement-report.txt"
if grep -q 'VERDICT: FAIL' "$SCRATCH/refinement-report.txt"; then
  log "FAIL agile-refinement: VERDICT FAIL"
  exit 1
fi
if ! grep -q 'VERDICT: PASS' "$SCRATCH/refinement-report.txt"; then
  log "FAIL agile-refinement: no VERDICT PASS"
  exit 1
fi
log "PASS /agile-refinement Mode 1 (0 red flags)"

# --- refinement-lint.py oracle (same checklist, exit-code check) ---
ORACLE="$SCRATCH/refinement-lint-oracle.txt"
python3 planning/edger/scripts/refinement-lint.py --scope planning/edger --round run-gates-oracle >"$ORACLE" 2>&1
if grep -q '\[RED\]' "$ORACLE"; then
  log "FAIL refinement-lint.py oracle"
  cat "$ORACLE" >>"$SCRATCH/run-gates.log"
  exit 1
fi
log "PASS refinement-lint.py oracle"

# --- path preflight ---
set +e
bash planning/edger/scripts/path-preflight.sh . 2>&1 | tee "$SCRATCH/path-preflight.txt"
PF_EXIT=${PIPESTATUS[0]}
set -e
MISSING=$(grep '^Missing:' "$SCRATCH/path-preflight.txt" | awk '{print $2}')
if [[ "$PF_EXIT" -ne 0 || "${MISSING:-1}" != "0" ]]; then
  log "FAIL path-preflight missing=$MISSING"
  exit 1
fi
log "PASS path-preflight"

# --- operation/deploy layout contract ---
set +e
python3 planning/edger/scripts/deploy-layout-check.py --repo . 2>&1 | tee "$SCRATCH/deploy-layout-check.txt"
DL_EXIT=${PIPESTATUS[0]}
set -e
[[ "$DL_EXIT" -eq 0 ]] || { log "FAIL deploy-layout-check"; exit 1; }
log "PASS deploy-layout-check"

# --- local extension/module validation ---
set +e
python3 planning/edger/scripts/extension-validation.py --repo . --module gateway 2>&1 | tee "$SCRATCH/extension-validation.txt"
EXT_EXIT=${PIPESTATUS[0]}
set -e
[[ "$EXT_EXIT" -eq 0 ]] || { log "FAIL extension-validation"; exit 1; }
log "PASS extension-validation"

# --- story section inspection ---
python3 - <<'PY' | tee "$SCRATCH/artifact-inspection.txt"
import pathlib, re, sys
root = pathlib.Path("planning/edger/epics")
required = ["## Context", "## Files", "## Detail", "## Tasks", "## Verification"]
missing = []
n = 0
for epic in sorted(root.iterdir()):
    if not epic.is_dir():
        continue
    for story in sorted(epic.glob("*.md")):
        if story.name in ("00-overview.md", "spike.md"):
            continue
        if not re.match(r"^\d{2}-.+\.md$", story.name):
            continue
        n += 1
        text = story.read_text()
        for sec in required:
            if sec not in text:
                missing.append(f"{story}: {sec}")
print(f"epics: {sum(1 for d in root.iterdir() if d.is_dir())}")
print(f"stories: {n}")
if missing:
    for m in missing:
        print(f"FAIL {m}")
    sys.exit(1)
print("PASS — all stories have required sections")
PY
log "PASS artifact inspection"

# --- WebIDE design-system and build contract ---
bash planning/edger/scripts/webide-ui-gate.sh 2>&1 | tee "$SCRATCH/webide-ui-gate.txt"
log "PASS webide-ui-gate"

# --- optional JS tests + cargo check ---
ROOT_JS_TESTS=$(find . -maxdepth 2 \( -name '*.test.ts' -o -name '*.spec.ts' \) -not -path './target/*' -print | sort)
if [[ -n "$ROOT_JS_TESTS" ]]; then
  set +e
  bun test 2>&1 | tee "$SCRATCH/bun-test.txt"
  BUN_EXIT=${PIPESTATUS[0]}
  set -e
  [[ "$BUN_EXIT" -eq 0 && $(grep -c '0 fail' "$SCRATCH/bun-test.txt") -ge 1 ]] || { log "FAIL bun test"; exit 1; }
  log "PASS bun test"
  BUN_STATUS='{"status":"passed","fail":0}'
else
  printf 'bun test skipped: no root JS/TS test suite exists after Bun adapter removal\n' | tee "$SCRATCH/bun-test.txt"
  log "SKIP bun test (no root JS/TS test suite)"
  BUN_STATUS='{"status":"skipped","reason":"no root JS/TS test suite"}'
fi

set +e
cargo check --workspace 2>&1 | tee "$SCRATCH/cargo-check.txt"
CARGO_EXIT=${PIPESTATUS[0]}
set -e
[[ "$CARGO_EXIT" -eq 0 ]] || { log "FAIL cargo check"; exit 1; }
log "PASS cargo check"

find planning/edger/epics -name '*.md' | sort >"$SCRATCH/epics-tree.txt"
python3 - <<'PY' >"$SCRATCH/epics-inventory.txt"
import pathlib, re
root = pathlib.Path("planning/edger/epics")
for epic in sorted(root.iterdir()):
    if not epic.is_dir():
        continue
    n = sum(1 for f in epic.glob("*.md") if re.match(r"^\d{2}-.+\.md$", f.name))
    print(f"{epic.name}: {n} stories")
PY

python3 - <<PY >"$SCRATCH/gates-summary.json"
import json, datetime
print(json.dumps({
  "passed_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
  "planning_lint": {
    "tool": "/agile-refinement Mode 1",
    "scope": "planning/edger/",
    "red_flags": 0,
    "oracle": "refinement-lint.py"
  },
  "path_preflight": {"missing": 0},
  "extension_validation": {"status": "passed", "module": "gateway"},
  "bun_test": $BUN_STATUS,
  "memory_lint": {"excluded": True, "reason": "server stability — operator directive 2026-06-29"}
}, indent=2))
PY

touch "$SCRATCH/gates.ok"
log "ALL PLANNING GATES PASS"
exit 0
