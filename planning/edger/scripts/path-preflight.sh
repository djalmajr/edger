#!/usr/bin/env bash
# Verify all planning/edger/* path refs in markdown exist under repo root.
set -euo pipefail
if ! command -v rg >/dev/null 2>&1; then
  echo "path-preflight: ripgrep (rg) is required but was not found in PATH" >&2
  exit 2
fi
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