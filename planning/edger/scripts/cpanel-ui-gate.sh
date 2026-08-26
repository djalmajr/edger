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
require '/files/download' "$SOURCE/main.tsx"
require 'PaginationControls' "$SOURCE/main.tsx"
require 'Math\.min\(\(page \+ 1\) \* pageSize, visible\.length\)' "$SOURCE/main.tsx"
require 'Rows per page' "$SOURCE/components/data-grid.tsx"
require 'PAGE_SIZE_OPTIONS = \[15, 30, 60\]' "$SOURCE/components/data-grid.tsx"
require 'contentClassName="w-max! min-w-max! max-w-none!"' "$SOURCE/components/data-grid.tsx"
require 'triggerClassName="h-8 w-fit"' "$SOURCE/components/data-grid.tsx"
require 'PageActionsContext' "$SOURCE/main.tsx"
require 'createPortal' "$SOURCE/main.tsx"
require 'ref=\{setPageActionsElement\}' "$SOURCE/main.tsx"
require 'data-slot="header-preferences"' "$SOURCE/main.tsx"
require 'data-slot="page-actions"' "$SOURCE/main.tsx"
require 'LanguageMenu' "$SOURCE/main.tsx"
require 'ThemeMenu' "$SOURCE/main.tsx"
require 'AccountMenu' "$SOURCE/main.tsx"
require 'I18nProvider' "$SOURCE/main.tsx"
require 'edger\.cpanel\.locale' "$SOURCE/lib/i18n.tsx"
reject 'SidebarFooter' "$SOURCE/main.tsx"
require 'Requests · 5 min' "$SOURCE/components/overview.tsx"
require 'Needs attention' "$SOURCE/components/overview.tsx"
require 'ChevronRightIcon' "$SOURCE/components/overview.tsx"
require 'Workers at a glance' "$SOURCE/components/overview.tsx"
require '/api/admin/observability/events\?limit=5' "$SOURCE/components/overview.tsx"
require 'apiDownload' "$SOURCE/lib/api.ts"
reject '(Open Observability|Updated |Live · 5 seconds|worker versions in the metrics snapshot)' "$SOURCE/components/overview.tsx"
reject '(lucide-react|iconify-icon|htm/preact|NativeSelect)' "$SOURCE"

(cd "$ROOT/workers" && bun run --filter '@edger/cpanel' build)
# Fingerprinted since 0.2.3: assets/<name>-<hash>.<ext> — the runtime pins
# these as immutable while the HTML shell stays no-cache.
require 'src="\./assets/index-[A-Za-z0-9_-]*\.js"' "$DIST/index.html"
require 'href="\./assets/index-[A-Za-z0-9_-]*\.css"' "$DIST/index.html"
# The font is fingerprinted under assets/ too — and this check must FAIL
# LOUDLY: a bare `test -s` under set -e died silently and hid the cause.
ls "$DIST"/assets/noto-sans-latin-wght-normal-*.woff2 >/dev/null 2>&1 \
  || { echo "missing fingerprinted noto-sans woff2 in dist/assets" >&2; exit 1; }

echo "cpanel-ui-gate ok"
