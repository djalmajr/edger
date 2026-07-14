#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SOURCE="$ROOT/workers/core/webide/src"
DIST="$ROOT/workers/core/webide/dist"
PACKAGE="$ROOT/workers/core/webide/package.json"
CONFIG="$ROOT/workers/core/webide/vite.config.js"

require() {
  local pattern="$1"
  local file="$2"
  rg -q -- "$pattern" "$file" || { echo "missing $pattern in ${file#$ROOT/}" >&2; exit 1; }
}

reject() {
  local pattern="$1"
  local path="$2"
  if rg -q -- "$pattern" "$path"; then
    echo "forbidden $pattern in ${path#$ROOT/}" >&2
    exit 1
  fi
}

require 'unplugin-icons/vite' "$CONFIG"
require 'base: "\./"' "$CONFIG"
require '@iconify-json/lucide' "$PACKAGE"
require 'unplugin-icons' "$PACKAGE"
require '~icons/lucide/search\?raw' "$SOURCE/icons.js"
require 'data-slot="dialog-content"' "$SOURCE/app.js"
require 'data-slot="tabs-list"' "$SOURCE/app.js"
require 'data-slot="context-menu-item"' "$SOURCE/app.js"
require 'data-slot="checkbox"' "$SOURCE/app.js"
require 'icon\("logo", 24\)' "$SOURCE/app.js"
require ':root\[data-theme="light"\] \{ --brand-icon: var\(--primary\); \}' "$SOURCE/styles.css"
require 'justify-content: flex-start' "$SOURCE/styles.css"
require '\.project-row:not\(\.project-head\):hover \{ background: var\(--accent\); \}' "$SOURCE/styles.css"
require 'class="project-row-link"' "$SOURCE/app.js"
require '\.project-row-link \{ position: absolute; inset: 0; z-index: 1;' "$SOURCE/styles.css"
require 'title="\$\{escapeHtml\(file\)\}" data-editor-tab=' "$SOURCE/app.js"
require 'src="\./app.js"' "$DIST/index.html"
require 'href="\./styles.css"' "$DIST/index.html"
reject 'Open cPanel' "$SOURCE"
reject 'Open cPanel' "$DIST"
reject 'const paths = \{' "$SOURCE/app.js"
reject 'list\.length\} project' "$SOURCE/app.js"
reject '\.project-name:hover strong' "$SOURCE/styles.css"
reject '<button class="project-name"' "$SOURCE/app.js"
reject '<strong>Preview</strong><span>' "$SOURCE/app.js"

bash "$ROOT/workers/core/webide/e2e/validate-flows.sh"

echo "webide-ui-gate ok"
