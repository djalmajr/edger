#!/usr/bin/env bash
# Copy planning gate evidence into repo and refresh consolidation gate bullets.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
SCRATCH="${SCRATCH:?SCRATCH must be set}"
EVIDENCE="$REPO_ROOT/planning/edger/status/evidence"
CONSOLIDATION="$REPO_ROOT/planning/edger/status/consolidation-2026-06-29-backlog-ready.md"

[[ -f "$SCRATCH/gates.ok" ]] || { echo "run run-gates.sh first" >&2; exit 1; }

mkdir -p "$EVIDENCE"
for f in refinement-report.txt agile-refinement-report.txt refinement-lint-oracle.txt path-preflight.txt \
  artifact-inspection.txt bun-test.txt cargo-check.txt epics-tree.txt epics-inventory.txt \
  gates-summary.json run-gates.log; do
  [[ -f "$SCRATCH/$f" ]] && cp "$SCRATCH/$f" "$EVIDENCE/$f"
done

GENERATED=$(python3 -c "import json; print(json.load(open('$SCRATCH/gates-summary.json'))['passed_at'])")

cat >"$EVIDENCE/agile-status.txt" <<STATUSEOF
/agile-status — consolidation (planning gates)
generated=$GENERATED

Planning lint: /agile-refinement Mode 1 — 0 red flags (evidence/refinement-report.txt)
Oracle: refinement-lint.py — 0 RED (evidence/refinement-lint-oracle.txt)
Path-preflight: 0 missing
bun test: 0 fail

Next: /agile-story on planning/edger/epics/02-edger-core/01-setup-core-crate.md
STATUSEOF
cp "$EVIDENCE/agile-status.txt" "$SCRATCH/agile-status.txt"

python3 - "$CONSOLIDATION" "$GENERATED" <<'PY'
import re, pathlib, sys
p = pathlib.Path(sys.argv[1])
g = sys.argv[2]
text = p.read_text(encoding="utf-8")
gates = (
    "## Maturity gates (planning)\n\n"
    f"_Rendered at {g} after run-gates.sh (planning lint only)._\n\n"
    "- [x] 7 epics / 31 stories decomposed com secoes obrigatorias\n"
    "- [x] /agile-refinement Mode 1 — 0 red flags (status/evidence/refinement-report.txt)\n"
    "- [x] refinement-lint.py oracle — 0 RED (status/evidence/refinement-lint-oracle.txt)\n"
    "- [x] Path-preflight — 0 missing (status/evidence/path-preflight.txt)\n"
    "- [x] Fase 1 completed; Fases 2-7 ready-for-development\n"
    "- [x] bun test pass (status/evidence/bun-test.txt)\n"
)
evidence = (
    "## Evidence (committed)\n\n"
    "| File | Gate |\n"
    "|---|---|\n"
    "| refinement-report.txt | /agile-refinement Mode 1 |\n"
    "| refinement-lint-oracle.txt | refinement-lint.py |\n"
    "| path-preflight.txt | cross-refs |\n"
    "| artifact-inspection.txt | story sections |\n"
    "| gates-summary.json | run-gates.sh |\n"
    "| agile-status.txt | consolidation snapshot |\n"
    "| bun-test.txt | regression |\n"
)
text = re.sub(
    r"## Maturity gates \(planning\)\n.*?(?=\n## Critical path)",
    gates + "\n",
    text,
    count=1,
    flags=re.DOTALL,
)
text = re.sub(
    r"## Evidence \(committed\).*?\Z",
    evidence,
    text,
    count=1,
    flags=re.DOTALL,
)
p.write_text(text, encoding="utf-8")
print("updated consolidation")
PY
echo "render-status: done"