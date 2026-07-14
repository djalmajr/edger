#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$ROOT/../../.." && pwd)"
FLOW_DIR="$ROOT/e2e/flows"
PERSONA_DIR="$ROOT/e2e/personas"
CATALOG="$ROOT/e2e/README.md"

personas=(
  webide-first-time-author
  webide-power-developer
  edger-platform-operator
  release-reliability-engineer
  adversarial-security-researcher
  ui-ux-responsive-auditor
  assistive-technology-developer
)

expected=(
  empty-dashboard-and-first-project
  dashboard-and-project-navigation
  project-template-catalog
  import-project-folder
  dashboard-project-lifecycle
  workbench-layout-and-navigation
  explorer-file-folder-lifecycle
  editor-tabs-and-order
  editor-autosave-and-persistence
  project-search
  lint-and-problems
  validation-and-safe-failure
  deploy-and-preview
  failed-deploy-preserves-preview
  footer-panels-and-order
  log-preservation
  operational-terminal
  layout-resizing-and-responsive
  keyboard-and-dialog-accessibility
)

fail() {
  echo "webide flow gate: $*" >&2
  exit 1
}

require() {
  local pattern="$1"
  local path="$2"
  rg -q -- "$pattern" "$path" || fail "missing '$pattern' in ${path#$ROOT/}"
}

actual_count="$(find "$FLOW_DIR" -maxdepth 1 -name '*.md' -type f | wc -l | tr -d ' ')"
[[ "$actual_count" == "${#expected[@]}" ]] || fail "expected ${#expected[@]} flows, found $actual_count"

persona_pattern="$(IFS='|'; echo "${personas[*]}")"
for persona in "${personas[@]}"; do
  persona_file="$PERSONA_DIR/$persona.md"
  [[ -f "$persona_file" ]] || fail "missing persona $persona"
  require "^id: $persona$" "$persona_file"
  require '^name: .+' "$persona_file"
  require "\`$persona\`" "$CATALOG"
done

for id in "${expected[@]}"; do
  file="$FLOW_DIR/$id.md"
  [[ -f "$file" ]] || fail "missing flow $id"
  [[ "$(sed -n '1p' "$file")" == "---" ]] || fail "$id has no frontmatter"
  require "^id: $id$" "$file"
  require '^name: .+' "$file"
  require '^reference: .+' "$file"
  require "^persona: ($persona_pattern)$" "$file"
  require '^entry: "http://127\.0\.0\.1:19080/webide"$' "$file"
  require '^preconditions:$' "$file"
  require '^## User goal$' "$file"
  require '^## Steps \(each step is a UI ACTION \+ the expected result\)$' "$file"
  require '^1\. .*entry point' "$file"
  require '^## Expected result$' "$file"
  step_count="$(rg -c '^[0-9]+\.' "$file")"
  (( step_count >= 5 )) || fail "$id needs at least five observable steps"
  expected_step=1
  while read -r step; do
    (( step == expected_step )) || fail "$id has non-sequential step $step; expected $expected_step"
    expected_step=$((expected_step + 1))
  done < <(sed -n 's/^\([0-9][0-9]*\)\..*/\1/p' "$file")
  if rg -q '^[0-9]+\..*(acesse|navegue para|vá para) https?://' "$file"; then
    fail "$id navigates directly to a URL instead of using the UI"
  fi
  require "\`$id\`" "$CATALOG"
  catalog_steps="$(awk -F '|' -v needle="\`$id\`" '$0 ~ needle { value=$5; gsub(/[[:space:]]/, "", value); print value }' "$CATALOG")"
  [[ "$catalog_steps" == "$step_count" ]] || fail "$id catalog says $catalog_steps steps, file has $step_count"
  reference="$(sed -n 's/^reference: //p' "$file")"
  [[ -f "$REPO_ROOT/$reference" ]] || fail "$id references missing file $reference"
  persona="$(sed -n 's/^persona: //p' "$file")"
  [[ -f "$PERSONA_DIR/$persona.md" ]] || fail "$id references missing persona $persona"
done

coverage=(
  'New project'
  'Import'
  'Duplicate project'
  'Rename project'
  'Delete project'
  'New file'
  'New folder'
  'menu de contexto'
  'tabs'
  'title'
  'arraste'
  'Ctrl/Cmd\+S'
  'autosave'
  'Ctrl/Cmd\+Shift\+F'
  'Match case'
  'regular expression'
  'Problems'
  'Validate project'
  'Deploy project'
  'Refresh preview'
  'Open in new tab'
  'Preserve logs'
  'Terminal'
  'help'
  'files'
  'status'
  'preview'
  'validate'
  'deploy'
  'clear'
  'splitter'
  'viewport'
)

for pattern in "${coverage[@]}"; do
  rg -qi -- "$pattern" "$FLOW_DIR" || fail "missing functional coverage for '$pattern'"
done

[[ -f "$ROOT/e2e/fixtures/import-static-spa/manifest.yaml" ]] || fail "missing valid import fixture"
[[ -f "$ROOT/e2e/fixtures/import-routes-table/manifest.yaml" ]] || fail "missing routes import fixture"
[[ -f "$ROOT/e2e/fixtures/import-fetch-handler/manifest.yaml" ]] || fail "missing fetch import fixture"
[[ -f "$ROOT/e2e/fixtures/import-missing-entrypoint/manifest.yaml" ]] || fail "missing invalid import fixture"

echo "webide-flow-gate ok (${#expected[@]} flows)"
