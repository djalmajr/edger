#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
APP="$ROOT/workers/core/cpanel"
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
require '@edger/ui/components/ui/sidebar' "$SOURCE/main.tsx"
require '@edger/ui/components/ui/chart' "$SOURCE/main.tsx"
require '@edger/ui/components/ui/select' "$SOURCE/main.tsx"
require '@edger/ui/icons/lucide' "$SOURCE/main.tsx"
require 'createRouter' "$SOURCE/main.tsx"
require 'QueryClientProvider' "$SOURCE/main.tsx"
require 'sessionStorage.setItem' "$SOURCE/main.tsx"
require '/api/admin/observability/events' "$SOURCE/main.tsx"
require '/api/admin/observability/series' "$SOURCE/main.tsx"
reject '(lucide-react|iconify-icon|htm/preact|NativeSelect)' "$SOURCE"

(cd "$ROOT/workers" && bun run --filter '@edger/cpanel' build)
require 'src="\./app.js"' "$DIST/index.html"
require 'href="\./styles.css"' "$DIST/index.html"
test -s "$DIST/noto-sans-latin-wght-normal.woff2"

echo "cpanel-ui-gate ok"
