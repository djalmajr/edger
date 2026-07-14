#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
SOURCE="${ROOT}/workers/core/cpanel/index.js"

required=(
  'Search apps'
  'Sort apps'
  'Collapsible'
  'requestDurationMsP95'
  'req/min'
  'Processes'
  'Traffic'
  'No traffic yet'
  'Default version required'
  'Control plane'
  'aria-expanded'
  'toggleGroup'
  'Filter apps'
  'Clear filters'
  'SelectTrigger'
  'Apps per page'
  'Showing ${pageStart + 1}'
  'interactive=${false}'
  'Process capacity:'
  'grid-cols-[70px_minmax(150px,1fr)_120px_144px_120px_130px_120px]'
  'Clear type filter'
  'Clear routing filter'
  'Clear health filter'
  'Routable'
  'runtime?.health?.status'
  'Unobserved'
  'Healthy'
  'Degraded'
  'Failing'
  'multiple onValueChange=${setKindFilter}'
  'Total requests'
  'Worker versions loaded by the runtime'
  'Distinct apps, each version on its own pathname'
  'Enabled worker versions'
  'Observed across worker versions'
  '<span>Pathname</span>'
  'visualSlots = Math.min(capacity, 8)'
  'icon="lucide:cpu"'
  'Disabled version'
  'Enabled version'
  'sm:w-72'
  'rolling 60-second window'
  'function HelpTooltip'
  'icon="lucide:circle-question-mark"'
  'align="start"'
  'align="end"'
  'function AttentionPopover'
  'Needs attention <${Badge}'
  'View all in Observability'
  'const closeOutside = (event) =>'
  'document.addEventListener("pointerdown", closeOutside)'
  'if (event.key === "Escape") setOpen(false)'
  'Open public URL in a new tab'
  'aria-label=${`Open URL ${url} in a new tab`}'
  'icon="lucide:scroll-text" size="15" /> View logs'
  'onViewFiles(worker, isLatest)'
  'const SESSION_API_KEY = "edger.cpanel.apiKey"'
  'sessionStorage.setItem(SESSION_API_KEY, apiKey)'
  'sessionStorage.removeItem(SESSION_API_KEY)'
  'Restoring session'
  'showApiKey ? "text" : "password"'
  'showApiKey ? "Hide root key" : "Show root key"'
  'showApiKey ? "lucide:eye-off" : "lucide:eye"'
  'block max-w-full truncate font-mono text-sm text-foreground/80'
  'icon="lucide:folder-open"'
  'title=${url}'
  'text-muted-foreground mt-1 truncate text-xs'
  'text-muted-foreground text-sm">${visibleGroups.length} of ${groups.length} apps'
  'runtime.activeProcesses ? "Active"'
  'runtime.idleProcesses ? "Idle"'
  'runtime.terminatingProcesses ? "Terminating" : "Cold"'
  'No traffic yet</span><${HelpTooltip}'
  'function kindIcon(kind)'
  'fetchhandler: "lucide:braces"'
  'staticspa: "lucide:panels-top-left"'
  'fullstack: "lucide:layers-3"'
  'routestable: "lucide:route"'
  'wasmmodule: "lucide:binary"'
  'Application type: ${kindFilterLabel(kind.toLowerCase())}'
  'function readCpanelRoute()'
  'function cpanelLocation(view, target, path = "")'
  'history[replace ? "replaceState" : "pushState"]'
  'window.addEventListener("popstate", handlePopState)'
  'setPath=${navigateFilesPath}'
  'view === "observability"'
  'view === "logs"'
  'Worker version sections'
  '<${TabsList}>'
  'View logs'
  'Recent in-memory events'
  'OTEL is not required.'
  '/api/admin/observability/events?${params}'
  '/api/admin/observability/series?${params}'
  'Live session · 5 minutes'
  'Run health check'
  'there is no periodic polling'
  'Filter by request ID'
  'Worker console'
  'Runtime events'
  'event.droppedCount'
  'event.truncated'
  'No local events match these filters.'
  'eventsState.nextCursor'
  'sm:grid-cols-2 sm:px-4 lg:grid-cols-[70px_minmax(150px,1fr)_120px_144px_120px_130px_120px]'
  'text-muted-foreground mb-1 block text-[11px] font-semibold tracking-wide uppercase lg:hidden'
  'hidden grid-cols-[70px_minmax(150px,1fr)_120px_144px_120px_130px_120px]'
  'const [viewReload, setViewReload] = useState(0)'
  'const refreshCurrentView = () =>'
  'className="responsive-sidebar"'
  'p-3 sm:p-6'
  'flex w-full flex-wrap items-center justify-end gap-2 sm:w-auto sm:flex-nowrap'
)

for token in "${required[@]}"; do
  rg --fixed-strings --quiet "${token}" "${SOURCE}" || {
    echo "missing cPanel UI contract: ${token}" >&2
    exit 1
  }
done

STYLE_SOURCE="${ROOT}/workers/core/cpanel/index.css"
for token in '@media (max-width: 1199px)' '.responsive-sidebar' 'width: 4rem !important'; do
  rg --fixed-strings --quiet "${token}" "${STYLE_SOURCE}" || {
    echo "missing responsive shell contract: ${token}" >&2
    exit 1
  }
done

if ! grep -Fq 'collapsed ? "4rem" : "12rem"' workers/core/cpanel/components/ui/sidebar.js; then
  echo "cpanel-ui-gate: expanded sidebar must remain 12rem wide" >&2
  exit 1
fi

TOOLTIP_SOURCE="${ROOT}/workers/core/cpanel/components/ui/tooltip.js"
for token in 'useState' 'onMouseEnter' 'onMouseLeave' 'break-words' 'whitespace-normal' 'open ? "visible opacity-100" : "hidden"'; do
  rg --fixed-strings --quiet "${token}" "${TOOLTIP_SOURCE}" || {
    echo "missing isolated tooltip contract: ${token}" >&2
    exit 1
  }
done

if rg --fixed-strings --quiet 'className="w-full"' "${SOURCE}"; then
  echo "informational tooltips must not use an entire data cell as their trigger" >&2
  exit 1
fi

if rg --fixed-strings --quiet '<${NativeSelect}' "${SOURCE}"; then
  echo "cPanel must use the composed shadcn Select, not NativeSelect" >&2
  exit 1
fi

if rg --fixed-strings --quiet 'overflow-x-auto' "${SOURCE}"; then
  echo "cPanel worker cards must not use internal horizontal scrolling" >&2
  exit 1
fi

if rg --fixed-strings --quiet 'latest →' "${SOURCE}"; then
  echo "cPanel app headers must show only the default version badge" >&2
  exit 1
fi

if rg --fixed-strings --quiet 'Serving' "${SOURCE}"; then
  echo "cPanel must not use Serving as a routing or health status" >&2
  exit 1
fi

if rg --fixed-strings --quiet 'RUNTIME_WORKER_DIRS' "${SOURCE}"; then
  echo "cPanel overview must describe loaded workers without exposing the internal directory variable" >&2
  exit 1
fi

if rg --fixed-strings --quiet 'min-w-40' "${ROOT}/workers/core/cpanel/components/ui/select.js"; then
  echo "shadcn Select triggers must size to their content" >&2
  exit 1
fi

if rg --fixed-strings --quiet 'grid-cols-[110px_minmax(0,1fr)' "${SOURCE}"; then
  echo "cPanel version column must remain compact" >&2
  exit 1
fi

for component in select.js dropdown-menu.js; do
  rg --fixed-strings --quiet 'handlePointerDown' "${ROOT}/workers/core/cpanel/components/ui/${component}" || {
    echo "${component} must close on outside pointer interaction" >&2
    exit 1
  }
done

rm -rf /tmp/edger-cpanel-ui-gate
bun build "${SOURCE}" --target=browser --outdir=/tmp/edger-cpanel-ui-gate \
  --external='~/*' --external=fflate --external=htm/preact \
  --external=preact --external=preact/hooks >/dev/null

echo "cpanel-ui-gate ok"
