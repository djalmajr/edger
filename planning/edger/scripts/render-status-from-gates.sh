#!/usr/bin/env bash
# Regenerate status/evidence from SCRATCH gate outputs. Run ONLY after run-gates.sh exits 0.
# Usage: SCRATCH=/path/to/scratch ./planning/edger/scripts/render-status-from-gates.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
SCRATCH="${SCRATCH:?SCRATCH must be set}"
EVIDENCE="$REPO_ROOT/planning/edger/status/evidence"
CONSOLIDATION="$REPO_ROOT/planning/edger/status/consolidation-2026-06-29-backlog-ready.md"

if [[ ! -f "$SCRATCH/gates.ok" ]]; then
  echo "render-status: $SCRATCH/gates.ok missing — run run-gates.sh first" >&2
  exit 1
fi

mkdir -p "$EVIDENCE"
# Remove stale hand-generated epic reports (superseded by run-gates.sh full-tree report)
rm -f "$EVIDENCE"/refinement-epic-*.txt

for f in memory-lint.txt refinement-report.txt path-preflight.txt bun-test.txt cargo-check.txt epics-tree.txt epics-inventory.txt gates-summary.json run-gates.log; do
  if [[ -f "$SCRATCH/$f" ]]; then
    cp "$SCRATCH/$f" "$EVIDENCE/$f"
  fi
done

SUMMARY="$SCRATCH/gates-summary.json"
TARGET=$(python3 -c "import json; print(json.load(open('$SUMMARY'))['memory_lint']['invocation_target'])")
GENERATED=$(python3 -c "import json; print(json.load(open('$SUMMARY'))['passed_at'])")

# agile-status.txt (derived from gates-summary.json only)
cat >"$EVIDENCE/agile-status.txt" <<EOF
agile-status consolidation (rendered from gates-summary.json)
generated=$GENERATED

Gate results (exit-code driven via run-gates.sh):
- refinement-lint.py scope=planning/edger/ → 0 RED (evidence/refinement-report.txt)
- memory_lint MCP workspace=djalmajr project=edger invocation_target=$TARGET → findings_count=0
- path-preflight → missing=0
- bun test → 0 fail

Next: /agile-story on planning/edger/epics/02-edger-core/01-setup-core-crate.md
EOF

cp "$EVIDENCE/agile-status.txt" "$SCRATCH/agile-status.txt"

# Regenerate maturity gates + evidence sections in consolidation (template-driven)
python3 - <<'PY' "$CONSOLIDATION" "$EVIDENCE" "$SUMMARY" "$GENERATED" "$TARGET"
import json, pathlib, sys

consolidation = pathlib.Path(sys.argv[1])
evidence = sys.argv[2]
summary = json.load(open(sys.argv[3]))
generated = sys.argv[4]
target = sys.argv[5]

text = consolidation.read_text(encoding="utf-8")

gates_section = f"""## Maturity gates (planning)

_Auto-rendered by render-status-from-gates.sh at {generated} (requires run-gates.sh exit 0)._

- [x] Cada fase do roadmap tem epic correspondente (`01`–`07`)
- [x] Cada epic tem `00-overview.md` + >=1 story file
- [x] Stories contêm tasks acionáveis e comandos de verificação (`cargo test`, `bun test`, launches)
- [x] `refinement-lint.py` scope `planning/edger/` — **0 RED** (`status/evidence/refinement-report.txt`; tool=refinement-lint.py, not skill invocation)
- [x] `memory_lint` scope `workspace=djalmajr` `project=edger` — **findings_count=0** via local `ai-memory serve` at `{target}` (`status/evidence/memory-lint.txt`; remote memory.djalmajr.dev not used)
- [x] Fase 1 permanece `completed`; Fases 2-7 `ready-for-development`
- [x] Cross-refs path-preflight — missing=0 (`status/evidence/path-preflight.txt`)
"""

evidence_section = f"""## Evidence (committed in repo)

_Auto-rendered from `$SCRATCH` gate outputs at {generated}._

| File | Tool / gate | Result |
|---|---|---|
| `refinement-report.txt` | `refinement-lint.py` | 0 RED |
| `memory-lint.txt` | `ai-memory memory_lint` MCP @ `{target}` | findings_count=0 |
| `path-preflight.txt` | `path-preflight.sh` | missing=0 |
| `gates-summary.json` | `run-gates.sh` | all gates pass |
| `run-gates.log` | `run-gates.sh` | execution log |
| `agile-status.txt` | `render-status-from-gates.sh` | derived snapshot |
| `bun-test.txt` | `bun test` | 0 fail |
| `cargo-check.txt` | `cargo check --workspace` | pass |
| `epics-tree.txt` | inventory | file listing |
| `epics-inventory.txt` | inventory | story counts |
"""

import re
text = re.sub(
    r"## Maturity gates \(planning\)\n.*?(?=\n## Critical path)",
    gates_section + "\n",
    text,
    count=1,
    flags=re.DOTALL,
)
text = re.sub(
    r"## Evidence \(committed in repo\)\n.*?\Z",
    evidence_section,
    text,
    count=1,
    flags=re.DOTALL,
)

# Remove stale deviation about manual fallback if present — replace deviations block tail
if "## Deviations from prior consolidation" in text:
    text = re.sub(
        r"- `memory_lint` remoto indisponível.*\n",
        "- Gate I/O decoupled: `run-gates.sh` + `render-status-from-gates.sh` (no hand-written PASS claims)\n",
        text,
    )

consolidation.write_text(text, encoding="utf-8")
print(f"Updated {consolidation}")
PY

echo "render-status: evidence copied to $EVIDENCE, consolidation updated"
exit 0