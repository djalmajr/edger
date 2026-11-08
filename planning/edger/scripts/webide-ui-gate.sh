#!/usr/bin/env bash
set -euo pipefail

# Git Bash (MSYS) rewrites arguments that start with "/" into Windows paths and
# hands native tools (rg.exe) POSIX-style "/c/..." paths they cannot resolve.
# Both settings are harmless outside MSYS.
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

if ! command -v rg >/dev/null 2>&1; then
  echo "webide-ui-gate: ripgrep (rg) is required but was not found in PATH" >&2
  exit 2
fi

# `pwd -W` (MSYS only) prints the native "C:/..." form; elsewhere plain `pwd`.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && { pwd -W 2>/dev/null || pwd; })"
APP="$ROOT/workers/core/webide"
SOURCE="$APP/src"
DIST="$APP/dist"

require() {
  local pattern="$1"
  local path="$2"
  rg -q -- "$pattern" "$path" || { echo "missing $pattern in ${path#$ROOT/}" >&2; exit 1; }
}

reject() {
  local pattern="$1"
  local path="$2"
  if rg -q -- "$pattern" "$path"; then
    echo "forbidden $pattern in ${path#$ROOT/}" >&2
    exit 1
  fi
}

require 'react' "$APP/package.json"
require 'vite build --watch' "$APP/package.json"
require 'unplugin-icons/vite' "$APP/vite.config.ts"
require 'base: "\./"' "$APP/vite.config.ts"
require '@edger/ui/components/ui/(sortable|dialog)' "$SOURCE"
require '@edger/ui/icons/lucide' "$SOURCE"
require 'createRouter' "$SOURCE/main.tsx"
require 'QueryClientProvider' "$SOURCE/main.tsx"
require 'SettingsDialog' "$SOURCE/components/workbench.tsx"
require 'SortableItemHandle' "$SOURCE/components/workbench.tsx"
reject '(lucide-react|iconify-icon|htm/preact|prompt\()' "$SOURCE"

(cd "$ROOT/workers" && bun run --filter '@edger/webide' build)
# Fingerprinted since 0.2.3: assets/<name>-<hash>.<ext> — the runtime pins
# these as immutable while the HTML shell stays no-cache.
require 'src="\./assets/index-[A-Za-z0-9_-]*\.js"' "$DIST/index.html"
require 'href="\./assets/index-[A-Za-z0-9_-]*\.css"' "$DIST/index.html"
ls "$DIST"/assets/noto-sans-latin-wght-normal-*.woff2 >/dev/null 2>&1 \
  || { echo "missing fingerprinted noto-sans woff2 in dist/assets" >&2; exit 1; }

# The flow validator is a separate script that relies on the default MSYS
# argument conversion (it builds its own /c/... paths), so run it without the
# overrides exported above.
env -u MSYS_NO_PATHCONV -u MSYS2_ARG_CONV_EXCL bash "$APP/e2e/validate-flows.sh"

echo "webide-ui-gate ok"
