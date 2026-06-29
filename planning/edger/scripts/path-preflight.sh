#!/usr/bin/env bash
# Verify all planning/edger/* path refs in markdown exist under repo root.
set -euo pipefail
REPO_ROOT="${1:-.}"
cd "$REPO_ROOT"
MISSING=0
while IFS= read -r ref; do
  if [[ ! -e "$ref" ]]; then
    echo "MISSING $ref"
    MISSING=$((MISSING + 1))
  else
    echo "OK $ref"
  fi
done < <(rg -o --no-filename 'planning/edger/[a-zA-Z0-9_./-]+\.md' planning/edger | sort -u)
echo "---"
echo "Total refs: $(rg -o --no-filename 'planning/edger/[a-zA-Z0-9_./-]+\.md' planning/edger | sort -u | wc -l | tr -d ' ')"
echo "Missing: $MISSING"
exit "$MISSING"