import { unzipSync, zipSync } from "fflate";
import { html } from "htm/preact";
import { render } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { Alert, AlertDescription, AlertTitle } from "~/components/ui/alert.js";
import { Badge } from "~/components/ui/badge.js";
import { Button } from "~/components/ui/button.js";
import { ChartContainer, LineChartPlaceholder } from "~/components/ui/chart.js";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "~/components/ui/collapsible.js";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "~/components/ui/card.js";
import {
  closeDialog,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  openDialog,
} from "~/components/ui/dialog.js";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "~/components/ui/dropdown-menu.js";
import { Icon } from "~/components/ui/icon.js";
import { InputGroup, InputGroupAddon, InputGroupInput } from "~/components/ui/input-group.js";
import { Label } from "~/components/ui/label.js";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "~/components/ui/select.js";
import { Separator } from "~/components/ui/separator.js";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from "~/components/ui/sidebar.js";
import { Spinner } from "~/components/ui/spinner.js";
import { TabsList, TabsTrigger } from "~/components/ui/tabs.js";
import { Tooltip } from "~/components/ui/tooltip.js";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "~/components/ui/table.js";

const VIEWS = [
  { description: "Runtime posture at a glance", icon: "lucide:gauge", id: "overview", title: "Overview" },
  { description: "Runtime worker inventory", icon: "lucide:cpu", id: "workers", title: "Workers" },
  { description: "Local runtime signals and logs", icon: "lucide:activity", id: "observability-overview", title: "Observability" },
];

const SESSION_API_KEY = "edger.cpanel.apiKey";
const CPANEL_BASE_PATH = "/cpanel";

function decodeLocationPart(value) {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function readCpanelRoute() {
  const parts = location.pathname.split("/").filter(Boolean);
  if (parts[0] !== "cpanel") return { view: "overview" };
  if (parts[1] === "observability" && parts[2] === "logs") return { view: "observability-global" };
  if (parts[1] === "observability") return { view: "observability-overview" };
  if (parts[1] !== "workers") return { view: "overview" };
  if (parts.length < 5 || !["files", "logs", "observability"].includes(parts[4])) return { view: "workers" };
  const view = parts[4];
  return {
    path: view === "files" ? parts.slice(5).map(decodeLocationPart).join("/") : "",
    target: { name: decodeLocationPart(parts[2]), version: decodeLocationPart(parts[3]) },
    view,
  };
}

function cpanelLocation(view, target, path = "") {
  if (view === "overview") return `${CPANEL_BASE_PATH}/`;
  if (view === "observability-overview") return `${CPANEL_BASE_PATH}/observability`;
  if (view === "observability-global") return `${CPANEL_BASE_PATH}/observability/logs`;
  if (view === "workers" || !target) return `${CPANEL_BASE_PATH}/workers`;
  if (view === "observability") {
    return `${CPANEL_BASE_PATH}/workers/${encodeURIComponent(target.name)}/${encodeURIComponent(target.version)}/observability`;
  }
  if (view === "logs") {
    return `${CPANEL_BASE_PATH}/workers/${encodeURIComponent(target.name)}/${encodeURIComponent(target.version)}/logs`;
  }
  const suffix = path ? `/${path.split("/").map(encodeURIComponent).join("/")}` : "";
  return `${CPANEL_BASE_PATH}/workers/${encodeURIComponent(target.name)}/${encodeURIComponent(target.version)}/files${suffix}`;
}

function readSessionApiKey() {
  try {
    return sessionStorage.getItem(SESSION_API_KEY) || "";
  } catch {
    return "";
  }
}

function storeSessionApiKey(apiKey) {
  try {
    if (apiKey) sessionStorage.setItem(SESSION_API_KEY, apiKey);
    else sessionStorage.removeItem(SESSION_API_KEY);
  } catch {
    // Storage can be unavailable in privacy-restricted browser contexts.
  }
}

function HelpTooltip({ align = "center", content, side = "top" }) {
  return html`
    <${Tooltip} align=${align} content=${content} side=${side}>
      <button
        aria-label="More information"
        class="text-muted-foreground hover:text-foreground inline-flex size-5 shrink-0 cursor-help items-center justify-center rounded-sm"
        type="button"
      >
        <${Icon} icon="lucide:circle-question-mark" size="14" />
      </button>
    <//>
  `;
}

async function apiJson(apiKey, path, init = {}) {
  const headers = new Headers(init.headers || {});
  headers.set("x-api-key", apiKey);
  const response = await fetch(path, { ...init, headers });
  const text = await response.text();
  const data = text ? JSON.parse(text) : {};
  if (!response.ok) {
    throw new Error(data.message || `${response.status} ${response.statusText}`);
  }
  return data;
}

async function loadAll(apiKey) {
  const session = await apiJson(apiKey, "/api/admin/session");
  const [workers, workerErrors, metricsStats] = await Promise.all([
    apiJson(apiKey, "/api/admin/workers").then((data) => data.workers || []),
    apiJson(apiKey, "/api/admin/workers/error-summary")
      .then((data) => data.summary || {})
      .catch(() => ({})),
    apiJson(apiKey, "/metrics/stats").catch(() => null),
  ]);
  return { metricsStats, principal: session.principal, workerErrors, workers };
}

// ExecutionKind serializes unit variants as strings ("FetchHandler") and
// data-carrying variants as objects ({ StaticSpa: {...} }); surface the name.
function kindLabel(kind) {
  if (kind == null) return "-";
  if (typeof kind === "string") return kind;
  if (typeof kind === "object") {
    const keys = Object.keys(kind);
    return keys.length ? keys[0] : "-";
  }
  return String(kind);
}

function kindFilterLabel(kind) {
  return {
    fetchhandler: "Fetch handler",
    fullstack: "Fullstack",
    routestable: "Routes table",
    staticspa: "Static SPA",
    wasmmodule: "Wasm module",
  }[kind] || kind;
}

function kindIcon(kind) {
  return {
    fetchhandler: "lucide:braces",
    fullstack: "lucide:layers-3",
    routestable: "lucide:route",
    staticspa: "lucide:panels-top-left",
    wasmmodule: "lucide:binary",
  }[String(kind || "").toLowerCase()] || "lucide:box";
}

function listText(value) {
  return Array.isArray(value) && value.length ? value.join(", ") : "-";
}

function Login({ onAuthenticated }) {
  const [error, setError] = useState(null);
  const [showApiKey, setShowApiKey] = useState(false);
  const [submitting, setSubmitting] = useState(false);

  const handleSubmit = async (event) => {
    event.preventDefault();
    const apiKey = String(new FormData(event.currentTarget).get("apiKey") || "").trim();
    if (!apiKey) return;
    setError(null);
    setSubmitting(true);
    try {
      const data = await loadAll(apiKey);
      onAuthenticated(apiKey, data);
    } catch (err) {
      setError(err.message);
    } finally {
      setSubmitting(false);
    }
  };

  return html`
    <div class="bg-background flex min-h-full w-full items-center justify-center p-4">
      <form class="border-border bg-background w-full max-w-md rounded-md border p-5 shadow-sm" onSubmit=${handleSubmit}>
        <div class="flex items-center gap-3">
          <div class="bg-primary/10 text-primary flex size-10 items-center justify-center rounded-md">
            <${Icon} icon="lucide:key-round" size="20" />
          </div>
          <div>
            <h1 class="text-base font-semibold">EdgeR cPanel</h1>
            <p class="text-muted-foreground text-sm">Enter the operator key to manage this runtime.</p>
          </div>
        </div>
        <${Label} className="mt-5 block" for="cpanel-api-key">Root key<//>
        <${InputGroup} className="mt-2">
          <${InputGroupInput} autocomplete="current-password" id="cpanel-api-key" name="apiKey" type=${showApiKey ? "text" : "password"} />
          <${InputGroupAddon} align="inline-end">
            <${Button}
              aria-label=${showApiKey ? "Hide root key" : "Show root key"}
              aria-pressed=${showApiKey}
              onClick=${() => setShowApiKey((visible) => !visible)}
              size="icon-sm"
              type="button"
              variant="ghost"
            >
              <${Icon} icon=${showApiKey ? "lucide:eye-off" : "lucide:eye"} size="16" />
            <//>
          <//>
        <//>
        ${error &&
        html`<div class="border-destructive/30 bg-destructive/10 text-destructive mt-3 rounded-md border px-3 py-2 text-sm">
          ${error}
        </div>`}
        <${Button} className="mt-4 w-full" disabled=${submitting} type="submit">
          ${submitting ? html`<${Spinner} size="16" />` : html`<${Icon} icon="lucide:log-in" size="16" />`}
          Connect
        <//>
      </form>
    </div>
  `;
}

function MetricCard({ help, icon, label, value }) {
  return html`
    <div class="border-border rounded-md border p-4">
      <div class="text-muted-foreground flex items-center gap-2 text-sm">
        <${Icon} icon=${icon} size="16" />
        <span>${label}</span>
      </div>
      <div class="mt-3 truncate text-lg font-semibold">${value}</div>
      ${help && html`<p class="text-muted-foreground mt-2 truncate text-xs">${help}</p>`}
    </div>
  `;
}

function DetailItem({ label, value }) {
  return html`
    <div class="border-border rounded-md border p-4">
      <div class="text-muted-foreground text-sm">${label}</div>
      <div class="mt-2 truncate text-sm font-medium">${value}</div>
    </div>
  `;
}

function Section({ action, children, description, title }) {
  return html`
    <${Card}>
      <${CardHeader}>
        <${CardTitle}>${title}<//>
        ${description && html`<${CardDescription}>${description}<//>`}
        ${action && html`<${CardAction}>${action}<//>`}
      <//>
      <${CardContent}>${children}<//>
    <//>
  `;
}

function SeriesCard({ data, emptyLabel = "No observations", label, value }) {
  const hasData = data.some((entry) => entry > 0);
  return html`
    <div class="border-border min-w-0 rounded-lg border p-4">
      <div class="flex items-start justify-between gap-3">
        <span class="text-muted-foreground text-sm">${label}</span>
        <strong class="text-sm">${value}</strong>
      </div>
      <${ChartContainer} className="mt-4 h-24 aspect-auto">
        ${hasData
          ? html`<${LineChartPlaceholder} className="text-primary" data=${data} />`
          : html`<div class="text-muted-foreground flex h-full items-center justify-center text-sm">${emptyLabel}</div>`}
      <//>
    </div>
  `;
}

function rollingRequestRate(points, bucketMs) {
  if (!points.length || !bucketMs) return 0;
  const bucketCount = Math.max(1, Math.ceil(60000 / bucketMs));
  const window = points.slice(-bucketCount);
  const requests = window.reduce((sum, point) => sum + (point.requestCount || 0), 0);
  return requests * (60000 / (window.length * bucketMs));
}

function AttentionPopover({ group, errorInfo, unhealthyVersions, disabledVersions, onOpenLogs, onOpenObservability, onViewAll }) {
  const id = `attention-${group.name.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
  const containerRef = useRef(null);
  const [open, setOpen] = useState(false);
  const reasons = [
    ...disabledVersions.map((worker) => ({
      action: () => onOpenLogs(worker),
      actionLabel: "View logs",
      description: `Version ${worker.version} is disabled and its versioned pathname is not routable.`,
      icon: "lucide:circle-slash-2",
      title: `Disabled version ${worker.version}`,
    })),
    ...unhealthyVersions.map((worker) => ({
      action: () => onOpenObservability(worker),
      actionLabel: "View signals",
      description: `${healthPresentation(worker.health?.status || "unobserved").label} passive health from recent real traffic.`,
      icon: "lucide:heart-pulse",
      title: `Health ${worker.version}`,
    })),
    ...(errorInfo?.count ? [{
      action: () => onOpenLogs(group.versions[0]),
      actionLabel: "View errors",
      description: `${errorInfo.count} recent dispatch error${errorInfo.count === 1 ? "" : "s"}${errorInfo.latest?.code ? ` · latest ${errorInfo.latest.code}` : ""}.`,
      icon: "lucide:circle-alert",
      title: "Dispatch errors",
    }] : []),
  ];
  const closeAnd = (action) => {
    setOpen(false);
    action();
  };

  useEffect(() => {
    if (!open) return undefined;
    const closeOutside = (event) => {
      if (!containerRef.current?.contains(event.target)) setOpen(false);
    };
    const closeOnEscape = (event) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", closeOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [open]);

  return html`
    <span class="relative inline-flex" ref=${containerRef}>
      <${Button}
        aria-label=${`Open ${group.name} attention details`}
        aria-expanded=${open}
        aria-controls=${id}
        className="h-6 rounded-full px-2.5 text-xs"
        onClick=${(event) => { event.stopPropagation(); setOpen((value) => !value); }}
        size="sm"
        type="button"
        variant="outline"
      >
        Needs attention <${Badge} className="ml-1 px-1.5" variant="secondary">${reasons.length}<//>
      <//>
      ${open && html`<div
        aria-label=${`${group.name} attention details`}
        class="border-border bg-popover text-popover-foreground absolute right-0 top-full z-[70] mt-1.5 max-h-80 w-[min(24rem,calc(100vw-2rem))] overflow-y-auto rounded-md border p-0 shadow-md"
        id=${id}
        onClick=${(event) => event.stopPropagation()}
        role="dialog"
      >
        <div class="border-border border-b p-3">
          <strong class="text-sm">Needs attention</strong>
          <p class="text-muted-foreground mt-1 text-sm">${reasons.length} reason${reasons.length === 1 ? "" : "s"} for ${group.name}</p>
        </div>
        <div class="grid gap-1 p-2">
          ${reasons.map((reason, index) => html`
            <div class="hover:bg-muted/60 rounded-md p-2" key=${`${reason.title}-${index}`}>
              <div class="flex items-start gap-2">
                <${Icon} className="text-muted-foreground mt-0.5 shrink-0" icon=${reason.icon} size="16" />
                <div class="min-w-0 flex-1">
                  <strong class="text-sm">${reason.title}</strong>
                  <p class="text-muted-foreground mt-0.5 text-sm">${reason.description}</p>
                  <button class="text-primary mt-1.5 cursor-pointer text-sm font-medium hover:underline" onClick=${() => closeAnd(reason.action)} type="button">${reason.actionLabel}</button>
                </div>
              </div>
            </div>
          `)}
        </div>
        <div class="border-border border-t p-2">
          <${Button} className="flex justify-center self-stretch" onClick=${() => closeAnd(onViewAll)} size="sm" type="button" variant="ghost">View all in Observability<//>
        </div>
      </div>`}
    </span>
  `;
}

function visibilityBadge(visibility) {
  const isPublic = visibility === "public";
  return html`<${Badge} variant=${isPublic ? "default" : "secondary"}>${visibility || "-"}<//>`;
}

function StatTile({ dot, icon, label, sub, tone, value }) {
  const border = tone === "warn" ? "border-amber-300/70 bg-amber-50/40" : "border-border";
  const numCls = tone === "warn" ? "text-amber-700" : "";
  return html`
    <div class=${`rounded-xl border ${border} p-4`}>
      <div class="text-muted-foreground flex items-center gap-2 text-[11px] font-semibold tracking-wide uppercase">
        ${dot ? html`<span class=${`inline-block size-2 rounded-full ${dot}`}></span>` : html`<${Icon} icon=${icon} size="15" />`}
        <span>${label}</span>
      </div>
      <div class=${`mt-3 text-3xl leading-none font-bold tracking-tight ${numCls}`}>${value}</div>
      ${sub && html`<div class="text-muted-foreground mt-2 text-sm">${sub}</div>`}
    </div>
  `;
}

function PoolStat({ label, unit, value }) {
  return html`
    <div class="grid gap-1">
      <span class="text-muted-foreground text-[11px] font-semibold tracking-wide uppercase">${label}</span>
      <span class="text-lg font-bold">${value}${unit && html`<span class="text-muted-foreground ml-0.5 text-sm font-medium">${unit}</span>`}</span>
    </div>
  `;
}

function ProcessCapacity({ runtime }) {
  if (!runtime?.maxProcesses) return null;

  const capacity = Math.max(1, Number(runtime.maxProcesses) || 1);
  const active = Math.min(Number(runtime.activeProcesses) || 0, capacity);
  const idle = Math.min(Number(runtime.idleProcesses) || 0, capacity - active);
  const terminating = Math.min(Number(runtime.terminatingProcesses) || 0, capacity - active - idle);
  const available = Math.max(0, capacity - active - idle - terminating);
  const label = `${active} active, ${idle} idle, ${terminating} terminating, ${available} available`;
  const visualSlots = Math.min(capacity, 8);

  return html`
    <span aria-label=${`Process capacity: ${label}`} class="flex h-4 shrink-0 items-end gap-0.5" role="img" title=${label}>
      ${Array.from({ length: visualSlots }, (_, index) => {
        const representedSlot = Math.floor((index * capacity) / visualSlots);
        const state = representedSlot < active ? "active" : representedSlot < active + idle ? "idle" : representedSlot < active + idle + terminating ? "terminating" : "available";
        const className = state === "active" ? "h-4 bg-emerald-500" : state === "idle" ? "bg-primary/45 h-3" : state === "terminating" ? "h-3 bg-amber-500" : "bg-muted-foreground/20 h-2";
        return html`<span class=${`w-1.5 rounded-sm ${className}`} key=${index}></span>`;
      })}
    </span>
  `;
}

function healthPresentation(status) {
  return {
    degraded: {
      className: "text-amber-700",
      dot: "bg-amber-500",
      help: "Recent traffic contains both successful and failed requests.",
      label: "Degraded",
    },
    failing: {
      className: "text-destructive",
      dot: "bg-destructive",
      help: "Recent traffic reached the consecutive failure threshold.",
      label: "Failing",
    },
    healthy: {
      className: "text-emerald-700",
      dot: "bg-emerald-500",
      help: "Recent observed requests completed without worker failures.",
      label: "Healthy",
    },
    unobserved: {
      className: "text-muted-foreground",
      dot: "bg-muted-foreground/40",
      help: "No requests were observed in the five-minute health window.",
      label: "Unobserved",
    },
  }[status] || healthPresentation("unobserved");
}

function healthHelp(health) {
  const presentation = healthPresentation(health?.status || "unobserved");
  const windowMinutes = Math.max(1, Math.round((health?.windowMs || 300000) / 60000));
  const sampleCount = health?.sampleCount || 0;
  const observed = health?.observedAtMs
    ? ` Last observed ${new Date(health.observedAtMs).toLocaleString()}.`
    : " No observation in the current window.";
  return `${presentation.help} ${windowMinutes}-minute window · ${sampleCount} sample${sampleCount === 1 ? "" : "s"}.${observed} In-memory observations reset when the runtime restarts.`;
}

function routingLabel(status) {
  return {
    default: "Default",
    disabled: "Disabled",
    enabled: "Enabled",
  }[status] || "Enabled";
}

function StatusRow({ label, value }) {
  return html`
    <div class="border-border/70 flex min-w-0 items-center justify-between gap-3 border-b py-2 last:border-0">
      <span class="text-muted-foreground shrink-0 text-sm">${label}</span>
      <span class="min-w-0 truncate text-right text-sm font-medium">${value}</span>
    </div>
  `;
}

function OverviewView({ data, onGoToWorkers }) {
  const { metricsStats, principal, workerErrors, workers } = data;
  const pool = metricsStats?.pool || {};
  const apps = new Set(workers.map((worker) => worker.name));
  const enabled = workers.filter((worker) => worker.status !== "disabled");
  const disabled = workers.filter((worker) => worker.status === "disabled");
  const errored = Object.entries(workerErrors || {}).filter(([, info]) => info && info.count > 0);
  const attention = disabled.length + errored.length;
  const cacheTotal = (pool.cacheHits || 0) + (pool.cacheMisses || 0);
  const cacheHit = cacheTotal ? Math.round((100 * (pool.cacheHits || 0)) / cacheTotal) : 100;
  const hot = (metricsStats?.workers || []).reduce((sum, worker) => sum + (worker.activeProcesses || 0), 0);
  const totalRequests = (metricsStats?.workers || []).reduce((sum, worker) => sum + (worker.requestTotal || worker.requests || 0), 0);

  return html`
    <div class="grid gap-4">
      <div class="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <${StatTile} icon="lucide:layout-grid" label="Workers" sub="Worker versions loaded by the runtime" value=${String(workers.length)} />
        <${StatTile} icon="lucide:box" label="Apps" sub="Distinct apps, each version on its own pathname" value=${String(apps.size)} />
        <${StatTile} icon="lucide:route" label="Routable" sub="Enabled worker versions" value=${String(enabled.length)} />
        <${StatTile} icon="lucide:activity" label="Total requests" sub="Observed across worker versions" value=${String(totalRequests)} />
      </div>

      <div class="grid gap-4 lg:grid-cols-3">
        <${Card} className="lg:col-span-2">
          <${CardHeader}>
            <${CardTitle}>Pool health<//>
            <${CardAction}><code class="text-muted-foreground text-[11px]">live · /metrics/stats</code><//>
          <//>
          <${CardContent}>
            <div class="grid grid-cols-2 gap-x-4 gap-y-5 sm:grid-cols-3">
              <${PoolStat} label="Hot processes" value=${String(hot)} />
              <${PoolStat} label="Idle workers" value=${String(pool.idleWorkers ?? 0)} />
              <${PoolStat} label="In-flight" value=${String(pool.activeRequests ?? 0)} />
              <${PoolStat} label="Cache hit" unit="%" value=${String(cacheHit)} />
              <${PoolStat} label="Spawn p50" unit="ms" value=${String(pool.spawnLatencyMsP50 ?? 0)} />
              <${PoolStat} label="Terminated" value=${String(pool.terminatedTotal ?? 0)} />
            </div>
          <//>
        <//>
        <${Card}>
          <${CardHeader}><${CardTitle}>Runtime status<//><//>
          <${CardContent} className="py-0">
            <${StatusRow} label="Principal" value=${principal?.name || "-"} />
            <${StatusRow} label="Role" value=${principal?.role || "-"} />
            <${StatusRow} label="Namespaces" value=${html`<code class="text-xs">${listText(principal?.namespaces)}</code>`} />
            <${StatusRow} label="Control plane" value="root-key gate" />
            <${StatusRow} label="Remote deploy" value=${html`<span class="text-muted-foreground">not included</span>`} />
          <//>
        <//>
      </div>

      <${Card}>
        <${CardHeader} className="pb-1">
          <${CardTitle}>
            <span class="flex items-center gap-2">Needs attention ${attention > 0 && html`<${Badge} variant="secondary">${attention}<//>`}</span>
          <//>
        <//>
        <${CardContent} className="py-0">
          ${attention === 0 &&
          html`<p class="text-muted-foreground py-4 text-sm">Everything looks healthy — no disabled versions or recent errors.</p>`}
          ${disabled.map(
            (worker) => html`
              <div class="border-border/70 flex items-center gap-3 border-b py-2 last:border-0" key=${`d-${worker.name}@${worker.version}`}>
                <span class="flex size-8 shrink-0 items-center justify-center rounded-md bg-amber-100 text-amber-700"><${Icon} icon="lucide:triangle-alert" size="16" /></span>
                <div class="min-w-0">
                  <div class="flex items-center gap-2 text-sm font-semibold">${worker.name}<code class="rounded bg-amber-100 px-1.5 py-0.5 text-[11px] text-amber-700">${worker.version}</code></div>
                  <p class="text-muted-foreground truncate text-xs">Disabled — this version returns 404.</p>
                </div>
                <${Button} className="ml-auto" onClick=${onGoToWorkers} size="sm" type="button" variant="outline">Review<//>
              </div>
            `,
          )}
          ${errored.map(
            ([name, info]) => html`
              <div class="border-border/70 flex items-center gap-3 border-b py-2 last:border-0" key=${`e-${name}`}>
                <span class="flex size-8 shrink-0 items-center justify-center rounded-md bg-destructive/10 text-destructive"><${Icon} icon="lucide:circle-alert" size="16" /></span>
                <div class="min-w-0">
                  <div class="text-sm font-semibold">${name}</div>
                  <p class="text-muted-foreground truncate text-xs">${info.count} recent dispatch error${info.count > 1 ? "s" : ""}${info.latest ? ` — ${info.latest.code}` : ""}.</p>
                </div>
                <${Button} className="ml-auto" onClick=${onGoToWorkers} size="sm" type="button" variant="outline">Review<//>
              </div>
            `,
          )}
        <//>
      <//>
    </div>
  `;
}

const DEPLOY_DIALOG_ID = "deploy-app-dialog";

function compareSemver(a, b) {
  const pa = String(a).split(".").map((n) => parseInt(n, 10) || 0);
  const pb = String(b).split(".").map((n) => parseInt(n, 10) || 0);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] || 0) !== (pb[i] || 0)) return (pa[i] || 0) - (pb[i] || 0);
  }
  return 0;
}

const WORKER_ERRORS_DIALOG_ID = "worker-errors-dialog";
const FILES_DIALOG_ID = "worker-files-dialog";
const OPERATIONAL_EVENT_DIALOG_ID = "operational-event-dialog";

function workerUrl(worker, isLatest) {
  const scoped = worker.namespace ? `@${worker.namespace}/${worker.name}` : worker.name;
  return isLatest ? `/${scoped}` : `/${scoped}@${worker.version}`;
}

function formatBytes(bytes) {
  if (!bytes) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${units[unit]}`;
}

function FilesView({ apiKey, onUpload, path, reload, setPath, status, target }) {
  const [listing, setListing] = useState({ loading: true });
  const [dragging, setDragging] = useState(false);
  const targetKey = target ? `${target.name}@${target.version}` : null;

  useEffect(() => {
    if (!target) return undefined;
    let cancelled = false;
    setListing({ loading: true });
    const query = new URLSearchParams({ path, version: target.version });
    apiJson(apiKey, `/api/admin/workers/${encodeURIComponent(target.name)}/files?${query}`)
      .then((data) => !cancelled && setListing({ entries: data.entries || [] }))
      .catch((err) => !cancelled && setListing({ error: err.message }));
    return () => {
      cancelled = true;
    };
  }, [apiKey, targetKey, path, reload]);

  const handleDrop = async (event) => {
    event.preventDefault();
    setDragging(false);
    const files = [...event.dataTransfer.files];
    if (files.length === 1 && files[0].name.endsWith(".zip")) {
      onUpload(new Uint8Array(await files[0].arrayBuffer()));
      return;
    }
    const fileMap = await collectDroppedFiles(event.dataTransfer);
    if (Object.keys(fileMap).length) onUpload(zipSync(fileMap));
  };

  const crumbs = path ? path.split("/") : [];

  return html`
    <div
      class="relative grid gap-4"
      onDragLeave=${() => setDragging(false)}
      onDragOver=${(event) => { event.preventDefault(); setDragging(true); }}
      onDrop=${handleDrop}
    >
      ${path &&
      html`<div class="text-muted-foreground flex flex-wrap items-center gap-1.5 text-sm">
        <button class="hover:text-foreground cursor-pointer" onClick=${() => setPath("")} type="button">${target?.name}</button>
        ${crumbs.map((crumb, index) =>
          index === crumbs.length - 1
            ? html`<span class="text-border">/</span><span class="text-foreground font-mono">${crumb}</span>`
            : html`<span class="text-border">/</span><button class="hover:text-foreground cursor-pointer font-mono" onClick=${() => setPath(crumbs.slice(0, index + 1).join("/"))} type="button">${crumb}</button>`,
        )}
      </div>`}
      ${status?.error &&
      html`<${Alert} variant="destructive"><${AlertTitle}>Upload failed<//><${AlertDescription}>${status.error}<//><//>`}
      <div class="border-border overflow-hidden rounded-lg border">
        <div class="text-muted-foreground bg-muted/40 border-border grid grid-cols-[1fr_90px] gap-4 border-b p-4 text-[11px] font-semibold tracking-wide uppercase">
          <span>Name</span><span class="text-right">Size</span>
        </div>
        <div class="max-h-[calc(100vh-260px)] overflow-y-auto">
          ${listing.loading && html`<div class="flex justify-center p-4"><${Spinner} size="18" /></div>`}
          ${listing.error && html`<div class="text-destructive p-4 text-sm">${listing.error}</div>`}
          ${listing.entries &&
          !listing.entries.length &&
          html`<div class="text-muted-foreground p-4 text-center text-sm">This folder is empty.</div>`}
          ${path &&
          listing.entries &&
          html`<button class="border-border/60 hover:bg-accent flex w-full items-center gap-2 border-b p-4 text-left text-sm" onClick=${() => setPath(crumbs.slice(0, -1).join("/"))} type="button"><${Icon} className="text-muted-foreground" icon="lucide:corner-left-up" size="16" /><span class="text-muted-foreground">..</span></button>`}
          ${listing.entries?.map((entry) => {
            const isDir = entry.kind === "dir";
            return html`
              <div
                class=${`border-border/60 grid grid-cols-[1fr_90px] items-center gap-4 border-b p-4 last:border-0 ${isDir ? "hover:bg-accent cursor-pointer" : ""}`}
                key=${entry.name}
                onClick=${isDir ? () => setPath(path ? `${path}/${entry.name}` : entry.name) : undefined}
              >
                <span class="flex items-center gap-2 truncate text-sm">
                  <${Icon} className=${isDir ? "text-amber-500" : "text-muted-foreground"} icon=${isDir ? "lucide:folder" : "lucide:file"} size="16" />
                  <span class="truncate font-mono">${entry.name}</span>
                </span>
                <span class="text-muted-foreground text-right text-xs">${isDir ? "—" : formatBytes(entry.size)}</span>
              </div>
            `;
          })}
        </div>
      </div>
      ${dragging &&
      html`<div class="border-primary bg-primary/10 pointer-events-none absolute inset-0 z-10 flex items-center justify-center rounded-lg border-2 border-dashed">
        <span class="text-primary flex items-center gap-2 text-sm font-medium"><${Icon} icon="lucide:upload-cloud" size="18" /> Drop to publish to ${target?.name}@${target?.version}</span>
      </div>`}
    </div>
  `;
}

function ObservabilityOverview({ apiKey, data, onOpenWorker, reload = 0 }) {
  const [snapshot, setSnapshot] = useState({ metrics: data.metricsStats, loading: true });

  useEffect(() => {
    let cancelled = false;
    let timer;
    const refresh = async () => {
      try {
        const [metrics, series] = await Promise.all([
          apiJson(apiKey, "/metrics/stats"),
          apiJson(apiKey, "/api/admin/observability/series?windowMs=300000&bucketMs=15000"),
        ]);
        if (!cancelled) setSnapshot({ metrics, series, refreshedAt: Date.now() });
      } catch (error) {
        if (!cancelled) {
          setSnapshot((current) => ({ ...current, error: error.message, loading: false, stale: Boolean(current.series) }));
        }
      }
    };
    refresh();
    timer = setInterval(refresh, 5000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [apiKey, reload]);

  const metrics = snapshot.metrics || {};
  const series = snapshot.series;
  const points = series?.points || [];
  const rates = points.map((point) => point.requestCount * (60000 / (series?.bucketMs || 15000)));
  const latencies = points.map((point) => point.durationP95Ms || 0);
  const failures = points.map((point) => point.errorCount || 0);
  const workerErrors = data.workerErrors || {};
  const ranked = (metrics.workers || [])
    .map((worker) => {
      const errors = workerErrors[worker.name]?.count || 0;
      const health = worker.health?.status || "unobserved";
      const score = errors * 100
        + (worker.timeoutTotal || 0) * 10
        + (worker.rejectedTotal || 0) * 5
        + (health === "failing" ? 80 : health === "degraded" ? 30 : 0)
        + Math.round((worker.requestDurationMsP95 || 0) / 100);
      return { ...worker, errors, health, score };
    })
    .filter((worker) => worker.score > 0)
    .sort((a, b) => b.score - a.score)
    .slice(0, 10);
  const active = (metrics.workers || []).reduce((sum, worker) => sum + (worker.activeProcesses || 0), 0);
  const idle = (metrics.workers || []).reduce((sum, worker) => sum + (worker.idleProcesses || 0), 0);
  const queued = (metrics.workers || []).reduce((sum, worker) => sum + (worker.queued || 0), 0);
  const latestRate = rollingRequestRate(points, series?.bucketMs || 15000);
  const latestP95 = [...latencies].reverse().find((value) => value > 0) || 0;
  const recentErrors = failures.reduce((sum, value) => sum + value, 0);

  return html`
    <div class="grid gap-4">
      ${snapshot.error && html`<${Alert} variant=${snapshot.stale ? "default" : "destructive"}><${AlertTitle}>${snapshot.stale ? "Showing the last reliable snapshot" : "Observability data unavailable"}<//><${AlertDescription}>${snapshot.error}<//><//>`}
      <div class="grid gap-4 sm:grid-cols-2 xl:grid-cols-5">
        <${MetricCard} help="Rolling 60-second window inside this live session" icon="lucide:gauge" label="Request rate" value=${`${latestRate.toFixed(1)} req/min`} />
        <${MetricCard} help="Latest available bucket in this live session" icon="lucide:timer" label="P95 latency" value=${latestP95 ? `${latestP95} ms` : "No data"} />
        <${MetricCard} help="Failed dispatches observed in this live session" icon="lucide:circle-alert" label="Recent errors" value=${String(recentErrors)} />
        <${MetricCard} help="Current process snapshot" icon="lucide:cpu" label="Processes" value=${`${active} active · ${idle} idle`} />
        <${MetricCard} help="Requests currently waiting for a process" icon="lucide:list-ordered" label="Queue pressure" value=${String(queued)} />
      </div>

      <${Section}
        action=${html`<span class="text-muted-foreground text-sm">${snapshot.loading ? "Loading…" : snapshot.stale ? "Stale" : "Live session · 5 minutes"}</span>`}
        description="Short, bounded series collected by this EdgeR instance. Runtime restart or eviction is shown as a partial window."
        title="Runtime signals"
      >
        <div class="grid gap-4 lg:grid-cols-3">
          <${SeriesCard} data=${rates} label="Request rate" value=${`${latestRate.toFixed(1)} req/min`} />
          <${SeriesCard} data=${latencies} label="P95 latency" value=${latestP95 ? `${latestP95} ms` : "No data"} />
          <${SeriesCard} data=${failures} label="Errors" value=${String(recentErrors)} />
        </div>
        ${series?.partialWindow && html`<p class="text-muted-foreground mt-3 text-sm">Partial window: the local store started or evicted events inside the selected range.</p>`}
      <//>

      <${Section} description="Ordered by recent errors, passive health degradation, queue outcomes and latency." title="Workers requiring attention">
        ${ranked.length === 0
          ? html`<p class="text-muted-foreground py-4 text-sm">No worker currently requires attention.</p>`
          : html`<div class="grid gap-2">
              ${ranked.map((worker) => html`
                <div class="border-border flex flex-wrap items-center gap-3 rounded-lg border p-3" key=${`${worker.name}@${worker.version}`}>
                  <div class="min-w-0 flex-1">
                    <div class="flex flex-wrap items-center gap-2"><strong class="truncate text-sm">${worker.name}</strong><${Badge} variant="secondary"><span class="font-mono">${worker.version}</span><//><span class=${`text-sm ${healthPresentation(worker.health).className}`}>${healthPresentation(worker.health).label}</span></div>
                    <p class="text-muted-foreground mt-1 text-sm">${worker.errors} errors · ${worker.timeoutTotal || 0} timeouts · ${worker.rejectedTotal || 0} rejected · p95 ${worker.requestDurationMsP95 || 0} ms</p>
                  </div>
                  <div class="flex gap-2">
                    <${Button} onClick=${() => onOpenWorker(worker, "observability")} size="sm" type="button" variant="outline">Overview<//>
                    <${Button} onClick=${() => onOpenWorker(worker, "logs")} size="sm" type="button" variant="outline">Logs<//>
                  </div>
                </div>
              `)}
            </div>`}
      <//>
    </div>
  `;
}

function WorkerObservabilityView({ apiKey, data, reload = 0, target }) {
  const worker = data.workers.find(
    (entry) => entry.name === target.name && entry.version === target.version,
  );
  const initialRuntime = (data.metricsStats?.workers || []).find(
    (entry) => entry.name === target.name && entry.version === target.version,
  );
  const [runtime, setRuntime] = useState(initialRuntime);
  const [seriesState, setSeriesState] = useState({ loading: true });
  const [healthCheckState, setHealthCheckState] = useState(null);
  const errors = data.workerErrors?.[target.name];
  const processTotal = runtime
    ? (runtime.activeProcesses || 0) + (runtime.idleProcesses || 0) + (runtime.terminatingProcesses || 0)
    : 0;
  const requests = runtime?.requestTotal ?? runtime?.requests ?? 0;
  const p95 = runtime?.requestDurationMsP95;
  const health = healthPresentation(runtime?.health?.status || "unobserved");
  const enabledVersions = data.workers
    .filter((entry) => entry.name === target.name && entry.status !== "disabled")
    .sort((a, b) => compareSemver(b.version, a.version));
  const routing = worker?.status === "disabled"
    ? "Disabled"
    : enabledVersions[0]?.version === worker?.version
      ? "Default"
      : "Enabled";
  const targetKey = `${target.name}@${target.version}`;

  useEffect(() => {
    let cancelled = false;
    let timer;
    const refresh = async () => {
      const params = new URLSearchParams({
        worker: target.name,
        version: target.version,
        windowMs: "300000",
        bucketMs: "15000",
      });
      try {
        const [metrics, series] = await Promise.all([
          apiJson(apiKey, "/metrics/stats"),
          apiJson(apiKey, `/api/admin/observability/series?${params}`),
        ]);
        if (cancelled) return;
        setRuntime((metrics.workers || []).find(
          (entry) => entry.name === target.name && entry.version === target.version,
        ));
        setSeriesState({ data: series, refreshedAt: Date.now() });
      } catch (error) {
        if (!cancelled) {
          setSeriesState((current) => ({ ...current, error: error.message, loading: false, stale: Boolean(current.data) }));
        }
      }
    };
    refresh();
    timer = setInterval(refresh, 5000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [apiKey, reload, targetKey]);

  const series = seriesState.data;
  const points = series?.points || [];
  const rates = points.map((point) => point.requestCount * (60000 / (series?.bucketMs || 15000)));
  const latencies = points.map((point) => point.durationP95Ms || 0);
  const failures = points.map((point) => point.errorCount || 0);
  const latestRate = rollingRequestRate(points, series?.bucketMs || 15000);
  const latestP95 = [...latencies].reverse().find((value) => value > 0) || 0;
  const windowErrors = failures.reduce((sum, value) => sum + value, 0);
  const runHealthCheck = async () => {
    setHealthCheckState({ loading: true });
    try {
      const result = await apiJson(
        apiKey,
        `/api/admin/workers/${encodeURIComponent(target.name)}/health-check?version=${encodeURIComponent(target.version)}`,
        { method: "POST" },
      );
      setHealthCheckState({ result });
    } catch (error) {
      setHealthCheckState({ error: error.message });
    }
  };

  return html`
    <div class="grid gap-4">
      ${seriesState.error && html`<${Alert} variant=${seriesState.stale ? "default" : "destructive"}><${AlertTitle}>${seriesState.stale ? "Showing the last reliable snapshot" : "Observability data unavailable"}<//><${AlertDescription}>${seriesState.error}<//><//>`}
      ${healthCheckState?.result && html`<${Alert} variant=${healthCheckState.result.healthy ? "default" : "destructive"}><${AlertTitle}>Manual health check ${healthCheckState.result.healthy ? "passed" : "failed"}<//><${AlertDescription}>${healthCheckState.result.method} ${healthCheckState.result.path} · ${healthCheckState.result.status || healthCheckState.result.code || "No status"} · ${healthCheckState.result.durationMs} ms. This explicit probe is separate from passive health.<//><//>`}
      ${healthCheckState?.error && html`<${Alert} variant="destructive"><${AlertTitle}>Health check could not run<//><${AlertDescription}>${healthCheckState.error}<//><//>`}
      <div class="grid gap-4 sm:grid-cols-2 xl:grid-cols-5">
        <${MetricCard}
          help="Administrative routing state for this version"
          icon="lucide:route"
          label="Routing"
          value=${routing}
        />
        <${MetricCard}
          help=${healthHelp(runtime?.health)}
          icon="lucide:heart-pulse"
          label="Health"
          value=${health.label}
        />
        <${MetricCard}
          help=${runtime?.maxProcesses ? `${runtime.maxProcesses} configured capacity` : "No warm processes"}
          icon="lucide:cpu"
          label="Processes"
          value=${runtime?.maxProcesses ? `${processTotal}/${runtime.maxProcesses}` : "Cold"}
        />
        <${MetricCard}
          help="Cumulative since runtime start"
          icon="lucide:activity"
          label="Requests"
          value=${String(requests)}
        />
        <${MetricCard}
          help="Latest observed request duration percentile"
          icon="lucide:timer"
          label="P95 latency"
          value=${p95 == null ? "No data" : `${p95} ms`}
        />
      </div>

      <${Section}
        action=${html`<span class="text-muted-foreground text-sm">${seriesState.loading ? "Loading…" : seriesState.stale ? "Stale" : "Live session · 5 minutes"}</span>`}
        description="Bounded in-memory series. Gaps and runtime resets are explicit; no external stack is required."
        title="Recent signals"
      >
        <div class="grid gap-4 lg:grid-cols-3">
          <${SeriesCard} data=${rates} label="Request rate" value=${`${latestRate.toFixed(1)} req/min`} />
          <${SeriesCard} data=${latencies} label="P95 latency" value=${latestP95 ? `${latestP95} ms` : "No data"} />
          <${SeriesCard} data=${failures} label="Failed requests" value=${String(windowErrors)} />
        </div>
        ${series?.partialWindow && html`<p class="text-muted-foreground mt-3 text-sm">The selected window began before this in-memory store or includes evicted data. Charts show only available observations.</p>`}
      <//>

      <${Section}
        action=${worker?.healthCheck
          ? html`<${Button} disabled=${healthCheckState?.loading} onClick=${runHealthCheck} size="sm" type="button" variant="outline">${healthCheckState?.loading ? html`<${Spinner} size="14" />` : html`<${Icon} icon="lucide:stethoscope" size="14" />`} Run health check<//>`
          : null}
        description="Current process and queue snapshot for the selected version. Health checks run only when explicitly requested or configured on deploy; there is no periodic polling."
        title="Capacity and pressure"
      >
        <div class="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          <${DetailItem} label="Worker" value=${target.name} />
          <${DetailItem} label="Version" value=${target.version} />
          <${DetailItem} label="Recent dispatch errors" value=${String(errors?.count || 0)} />
          <${DetailItem} label="Active processes" value=${String(runtime?.activeProcesses || 0)} />
          <${DetailItem} label="Idle processes" value=${String(runtime?.idleProcesses || 0)} />
          <${DetailItem} label="Queued requests" value=${String(runtime?.queued || 0)} />
          <${DetailItem} label="Rejected" value=${String(runtime?.rejectedTotal || 0)} />
          <${DetailItem} label="Timeouts" value=${String(runtime?.timeoutTotal || 0)} />
          <${DetailItem} label="Queue wait P95" value=${`${runtime?.waitMsP95 || 0} ms`} />
        </div>
      <//>

      <${Alert}>
        <${Icon} icon="lucide:info" size="16" />
        <${AlertTitle}>Local observability<//>
        <${AlertDescription}>
          This view uses EdgeR runtime data directly. Open Logs for correlated events and request or trace IDs. OTEL remains an optional export path, not the source of this screen.
        <//>
      <//>
    </div>
  `;
}

const EVENT_SOURCE_LABELS = {
  console: "Worker console",
  drain: "Process lifecycle",
  orchestrator: "Orchestrator",
  release: "Release / migration",
  runtime: "Runtime events",
};

function WorkerLogsView({ apiKey, reload = 0, target }) {
  const initialParams = new URLSearchParams(location.search);
  const scoped = Boolean(target);
  const [eventsState, setEventsState] = useState({ loading: true });
  const [level, setLevel] = useState(initialParams.get("level") || "all");
  const [requestId, setRequestId] = useState(initialParams.get("requestId") || "");
  const [source, setSource] = useState(initialParams.get("source") || "all");
  const [workerFilter, setWorkerFilter] = useState(target?.name || initialParams.get("worker") || "");
  const [versionFilter, setVersionFilter] = useState(target?.version || initialParams.get("version") || "");
  const [processFilter, setProcessFilter] = useState(initialParams.get("processId") || "");
  const [outcomeFilter, setOutcomeFilter] = useState(initialParams.get("outcome") || "");
  const [statusFilter, setStatusFilter] = useState(initialParams.get("status") || "");
  const [traceId, setTraceId] = useState(initialParams.get("traceId") || "");
  const [live, setLive] = useState(false);
  const [tailState, setTailState] = useState({ received: 0 });
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [selectedEvent, setSelectedEvent] = useState(null);
  const [selectedEventId, setSelectedEventId] = useState(initialParams.get("event") || "");
  const liveCursor = useRef(0);

  const eventParams = (extra = {}) => {
    const params = new URLSearchParams(extra);
    const selectedWorker = scoped ? target.name : workerFilter.trim();
    const selectedVersion = scoped ? target.version : versionFilter.trim();
    if (selectedWorker) params.set("worker", selectedWorker);
    if (selectedVersion) params.set("version", selectedVersion);
    if (level !== "all") params.set("level", level);
    if (requestId.trim()) params.set("requestId", requestId.trim());
    if (source !== "all") params.set("source", source);
    if (processFilter.trim()) params.set("processId", processFilter.trim());
    if (outcomeFilter.trim()) params.set("outcome", outcomeFilter.trim());
    if (statusFilter.trim()) params.set("status", statusFilter.trim());
    if (traceId.trim()) params.set("traceId", traceId.trim());
    return params;
  };

  useEffect(() => {
    const params = new URLSearchParams();
    if (level !== "all") params.set("level", level);
    if (requestId.trim()) params.set("requestId", requestId.trim());
    if (source !== "all") params.set("source", source);
    if (!scoped && workerFilter.trim()) params.set("worker", workerFilter.trim());
    if (!scoped && versionFilter.trim()) params.set("version", versionFilter.trim());
    if (processFilter.trim()) params.set("processId", processFilter.trim());
    if (outcomeFilter.trim()) params.set("outcome", outcomeFilter.trim());
    if (statusFilter.trim()) params.set("status", statusFilter.trim());
    if (traceId.trim()) params.set("traceId", traceId.trim());
    if (selectedEventId) params.set("event", selectedEventId);
    const base = scoped ? cpanelLocation("logs", target) : cpanelLocation("observability-global");
    history.replaceState(null, "", `${base}${params.size ? `?${params}` : ""}`);
  }, [level, outcomeFilter, processFilter, requestId, scoped, selectedEventId, source, statusFilter, target?.name, target?.version, traceId, versionFilter, workerFilter]);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      setEventsState((current) => ({ ...current, loading: true }));
      const params = eventParams({ limit: "100" });
      try {
        const page = await apiJson(apiKey, `/api/admin/observability/events?${params}`);
        if (!cancelled) {
          liveCursor.current = Math.max(
            liveCursor.current,
            ...(page.events || []).map((event) => Number(event.id) || 0),
          );
          setEventsState({ ...page, loading: false });
        }
      } catch (error) {
        if (!cancelled) setEventsState({ error: error.message, loading: false });
      }
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [apiKey, level, outcomeFilter, processFilter, reload, requestId, scoped, source, statusFilter, target?.name, target?.version, traceId, versionFilter, workerFilter]);

  useEffect(() => {
    if (!live) return undefined;
    const controller = new AbortController();
    const run = async () => {
      const params = eventParams({ cursor: String(liveCursor.current), limit: "200" });
      try {
        const response = await fetch(`/api/admin/observability/events/stream?${params}`, {
          headers: { Accept: "text/event-stream", "x-api-key": apiKey },
          signal: controller.signal,
        });
        if (!response.ok || !response.body) throw new Error(`${response.status} ${response.statusText}`);
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        while (!controller.signal.aborted) {
          const { done, value } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });
          const frames = buffer.split("\n\n");
          buffer = frames.pop() || "";
          for (const frame of frames) {
            const eventType = frame.split("\n").find((line) => line.startsWith("event:"))?.slice(6).trim();
            const data = frame.split("\n").filter((line) => line.startsWith("data:")).map((line) => line.slice(5).trim()).join("\n");
            if (!data) continue;
            const payload = JSON.parse(data);
            if (eventType === "gap") {
              setTailState((current) => ({ ...current, gap: payload }));
              continue;
            }
            if (eventType !== "operational_event") continue;
            liveCursor.current = Math.max(liveCursor.current, Number(payload.id) || 0);
            setEventsState((current) => ({
              ...current,
              events: [payload, ...(current.events || []).filter((event) => event.id !== payload.id)].slice(0, 200),
              loading: false,
            }));
            setTailState((current) => ({ ...current, received: current.received + 1 }));
          }
        }
      } catch (error) {
        if (!controller.signal.aborted) setTailState((current) => ({ ...current, error: error.message }));
      }
    };
    run();
    return () => controller.abort();
  }, [apiKey, level, live, outcomeFilter, processFilter, requestId, scoped, source, statusFilter, target?.name, target?.version, traceId, versionFilter, workerFilter]);

  const loadOlder = async () => {
    if (!eventsState.nextCursor || loadingOlder) return;
    setLoadingOlder(true);
    try {
      const params = eventParams({ before: String(eventsState.nextCursor), limit: "100" });
      const page = await apiJson(apiKey, `/api/admin/observability/events?${params}`);
      setEventsState((current) => ({
        ...current,
        ...page,
        events: [...(current.events || []), ...(page.events || []).filter((event) => !(current.events || []).some((existing) => existing.id === event.id))],
        loading: false,
      }));
    } catch (error) {
      setEventsState((current) => ({ ...current, error: error.message }));
    } finally {
      setLoadingOlder(false);
    }
  };

  const showEvent = (event) => {
    setSelectedEvent(event);
    setSelectedEventId(String(event.id));
    requestAnimationFrame(() => openDialog(OPERATIONAL_EVENT_DIALOG_ID)());
  };
  useEffect(() => {
    if (!selectedEventId || selectedEvent) return;
    const restored = (eventsState.events || []).find((event) => String(event.id) === selectedEventId);
    if (!restored) return;
    setSelectedEvent(restored);
    requestAnimationFrame(() => openDialog(OPERATIONAL_EVENT_DIALOG_ID)());
  }, [eventsState.events, selectedEvent, selectedEventId]);
  const closeEvent = () => {
    closeDialog(OPERATIONAL_EVENT_DIALOG_ID)();
    setSelectedEvent(null);
    setSelectedEventId("");
  };
  const tableColSpan = scoped ? 10 : 11;

  return html`
    <div class="grid gap-4">
      <${Alert}>
        <${Icon} icon="lucide:database" size="16" />
        <${AlertTitle}>Recent in-memory events<//>
        <${AlertDescription}>
          Local runtime events are bounded to ${eventsState.stats?.capacity || 2000} entries and reset when EdgeR restarts. OTEL is not required.
          ${(eventsState.stats?.dropped || eventsState.stats?.truncated) ? html`
            <span class="mt-1 block">
              Capture limits observed: ${eventsState.stats.dropped || 0} dropped lines and ${eventsState.stats.truncated || 0} truncated lines.
            </span>
          ` : null}
        <//>
      <//>

      <div class="flex flex-wrap items-center gap-2">
        <${Select} onValueChange=${setLevel} value=${level}>
          <${SelectTrigger}><${Icon} icon="lucide:list-filter" size="15" /><${SelectValue}>${level === "all" ? "All levels" : level[0].toUpperCase() + level.slice(1)}<//><//>
          <${SelectContent}>
            <${SelectGroup}>
              <${SelectItem} value="all">All levels<//>
              <${SelectItem} value="info">Info<//>
              <${SelectItem} value="warn">Warn<//>
              <${SelectItem} value="error">Error<//>
            <//>
          <//>
        <//>
        <${Select} onValueChange=${setSource} value=${source}>
          <${SelectTrigger}><${Icon} icon="lucide:radio" size="15" /><${SelectValue}>${source === "all" ? "All sources" : EVENT_SOURCE_LABELS[source] || source}<//><//>
          <${SelectContent}>
            <${SelectGroup}>
              <${SelectItem} value="all">All sources<//>
              <${SelectItem} value="runtime">Runtime events<//>
              <${SelectItem} value="console">Worker console<//>
              <${SelectItem} value="release">Release / migration<//>
              <${SelectItem} value="drain">Process lifecycle<//>
            <//>
          <//>
        <//>
        <${InputGroup} className="w-full sm:w-72">
          <${InputGroupAddon}><${Icon} icon="lucide:search" size="15" /><//>
          <${InputGroupInput} aria-label="Filter by request ID" onInput=${(event) => setRequestId(event.currentTarget.value)} placeholder="Filter by request ID" value=${requestId} />
        <//>
        <${Button} className="sm:ml-auto" onClick=${() => setLive((value) => !value)} size="sm" type="button" variant=${live ? "default" : "outline"}>
          <${Icon} icon=${live ? "lucide:pause" : "lucide:radio"} size="14" /> ${live ? "Pause live" : "Start live"}
        <//>
        ${tailState.received ? html`<${Badge} variant="secondary">${tailState.received} live<//>` : null}
      </div>

      ${!scoped && html`
        <div class="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
          <${InputGroup}><${InputGroupInput} aria-label="Filter by worker" onInput=${(event) => setWorkerFilter(event.currentTarget.value)} placeholder="Worker" value=${workerFilter} /><//>
          <${InputGroup}><${InputGroupInput} aria-label="Filter by version" onInput=${(event) => setVersionFilter(event.currentTarget.value)} placeholder="Version" value=${versionFilter} /><//>
          <${InputGroup}><${InputGroupInput} aria-label="Filter by process ID" onInput=${(event) => setProcessFilter(event.currentTarget.value)} placeholder="Process ID" value=${processFilter} /><//>
          <${InputGroup}><${InputGroupInput} aria-label="Filter by outcome" onInput=${(event) => setOutcomeFilter(event.currentTarget.value)} placeholder="Outcome" value=${outcomeFilter} /><//>
          <${InputGroup}><${InputGroupInput} aria-label="Filter by HTTP status" onInput=${(event) => setStatusFilter(event.currentTarget.value)} placeholder="HTTP status" value=${statusFilter} /><//>
          <${InputGroup}><${InputGroupInput} aria-label="Filter by trace ID" onInput=${(event) => setTraceId(event.currentTarget.value)} placeholder="Trace ID" value=${traceId} /><//>
        </div>
      `}

      ${tailState.gap ? html`<${Alert} variant="destructive"><${Icon} icon="lucide:triangle-alert" size="16" /><${AlertTitle}>Retention gap detected<//><${AlertDescription}>The requested cursor expired. Resumed from event ${tailState.gap.oldestAvailable || "the oldest retained event"}.<//><//>` : null}
      ${tailState.error ? html`<${Alert} variant="destructive"><${AlertTitle}>Live tail disconnected<//><${AlertDescription}>${tailState.error}<//><//>` : null}

      <div class="border-border overflow-hidden rounded-lg border">
        <${Table}>
          <${TableHeader}>
            <${TableRow}>
              <${TableHead}>Time<//>${!scoped && html`<${TableHead}>Worker @ version<//>`}<${TableHead}>Source<//><${TableHead}>Level<//><${TableHead}>Event<//><${TableHead}>Outcome<//><${TableHead}>Status<//><${TableHead}>Duration<//><${TableHead}>Process ID<//><${TableHead}>Request ID<//><${TableHead}><span class="sr-only">Actions</span><//>
            <//>
          <//>
          <${TableBody}>
            ${eventsState.loading && html`<${TableRow}><${TableCell} className="py-8 text-center" colSpan=${tableColSpan}><${Spinner} size="18" /><//><//>`}
            ${eventsState.error && html`<${TableRow}><${TableCell} className="text-destructive py-8 text-center" colSpan=${tableColSpan}>${eventsState.error}<//><//>`}
            ${!eventsState.loading && !eventsState.error && !(eventsState.events || []).length && html`<${TableRow}><${TableCell} className="text-muted-foreground py-8 text-center" colSpan=${tableColSpan}>No local events match these filters.<//><//>`}
            ${(eventsState.events || []).map((event) => html`
              <${TableRow} key=${event.id}>
                <${TableCell} className="whitespace-nowrap">${new Date(Number(event.atMs)).toLocaleTimeString()}<//>
                ${!scoped && html`<${TableCell}><span class="font-medium">${event.worker || "—"}</span><span class="text-muted-foreground block font-mono text-xs">${event.version || "—"}</span><//>`}
                <${TableCell}>${EVENT_SOURCE_LABELS[event.source] || event.source}<//>
                <${TableCell}><${Badge} variant=${event.level === "error" ? "destructive" : "secondary"}>${event.level}<//><//>
                <${TableCell}><span class="font-medium">${event.kind}</span>${event.message ? html`<span class="text-muted-foreground block max-w-80 truncate text-xs" title=${event.message}>${event.message}</span>` : null}${event.code ? html`<span class="text-muted-foreground block text-xs">${event.code}</span>` : null}${event.truncated ? html`<${Badge} className="mt-1" variant="outline">Truncated<//>` : null}${event.droppedCount ? html`<${Badge} className="mt-1" variant="outline">${event.droppedCount} dropped<//>` : null}<//>
                <${TableCell} className="font-mono text-xs">${event.outcome || "—"}<//>
                <${TableCell}>${event.status ?? "—"}<//>
                <${TableCell}>${event.durationMs == null ? "—" : `${event.durationMs} ms`}<//>
                <${TableCell}><code class="block max-w-32 truncate text-xs" title=${event.processId || ""}>${event.processId || "—"}</code><//>
                <${TableCell}><code class="block max-w-48 truncate text-xs" title=${event.requestId || ""}>${event.requestId || "—"}</code><//>
                <${TableCell}><${Button} aria-label=${`View event ${event.id}`} onClick=${() => showEvent(event)} size="icon-sm" type="button" variant="ghost"><${Icon} icon="lucide:eye" size="14" /><//><//>
              <//>
            `)}
          <//>
        <//>
      </div>
      ${eventsState.nextCursor && html`<div class="flex justify-center"><${Button} disabled=${loadingOlder} onClick=${loadOlder} size="sm" type="button" variant="outline">${loadingOlder ? html`<${Spinner} size="14" />` : html`<${Icon} icon="lucide:chevrons-down" size="14" />`} Load older<//></div>`}

      <${Dialog} id=${OPERATIONAL_EVENT_DIALOG_ID} className="max-w-2xl" onClose=${() => { setSelectedEvent(null); setSelectedEventId(""); }}>
        <${DialogContent} className="max-h-[85vh] max-w-2xl overflow-y-auto">
          <${DialogHeader}><${DialogTitle}>Operational event${selectedEvent ? ` #${selectedEvent.id}` : ""}<//><${DialogDescription}>Allowlisted local event data for diagnosis and correlation.<//><//>
          ${selectedEvent && html`
            <dl class="grid gap-x-5 gap-y-3 text-sm sm:grid-cols-2">
              ${Object.entries(selectedEvent).filter(([, value]) => value !== null && value !== undefined && value !== "").map(([key, value]) => html`
                <div class=${key === "message" ? "sm:col-span-2" : ""} key=${key}>
                  <dt class="text-muted-foreground text-xs font-medium uppercase tracking-wide">${key}<//>
                  <dd class="mt-1 break-words font-mono">${typeof value === "object" ? JSON.stringify(value) : String(value)}
                    ${(key === "requestId" || key === "traceId") && html`<${Button} aria-label=${`Copy ${key}`} onClick=${() => navigator.clipboard?.writeText(String(value))} size="icon-sm" type="button" variant="ghost"><${Icon} icon="lucide:copy" size="13" /><//>`}
                  <//>
                </div>
              `)}
            </dl>
          `}
          <div class="flex justify-end"><${Button} onClick=${closeEvent} size="sm" type="button" variant="outline">Close<//></div>
        <//>
      <//>
    </div>
  `;
}

function WorkersView({ apiKey, data, onDeployed, onViewAllLogs, onViewFiles, onViewLogs, onViewObservability }) {
  const [busy, setBusy] = useState(null);
  const [errorsView, setErrorsView] = useState(null);
  const [kindFilter, setKindFilter] = useState([]);
  const [liveMetrics, setLiveMetrics] = useState(data.metricsStats);
  const [openGroups, setOpenGroups] = useState({});
  const [pageIndex, setPageIndex] = useState(0);
  const [pageSize, setPageSize] = useState(10);
  const [query, setQuery] = useState("");
  const [routingFilter, setRoutingFilter] = useState([]);
  const [sort, setSort] = useState("recent");
  const [healthFilter, setHealthFilter] = useState([]);
  const [traffic, setTraffic] = useState({});
  const trafficHistory = useRef({});
  const workerErrors = data.workerErrors || {};
  const runtimeWorkers = liveMetrics?.workers || [];

  useEffect(() => {
    let cancelled = false;
    const refreshMetrics = async () => {
      try {
        const next = await apiJson(apiKey, "/metrics/stats");
        if (cancelled) return;
        const now = Date.now();
        const nextTraffic = {};
        for (const worker of next.workers || []) {
          const key = `${worker.namespace || ""}/${worker.name}@${worker.version}`;
          const count = worker.requestTotal ?? worker.requests ?? 0;
          const samples = [...(trafficHistory.current[key] || []), { at: now, count }]
            .filter((sample) => now - sample.at <= 60000)
            .slice(-13);
          trafficHistory.current[key] = samples;
          const oldest = samples[0];
          const elapsedMinutes = oldest ? Math.max((now - oldest.at) / 60000, 1 / 60000) : 0;
          nextTraffic[key] = {
            p95: worker.requestDurationMsP95 || null,
            rate: samples.length < 2 ? null : Math.max(0, (count - oldest.count) / elapsedMinutes),
            total: count,
          };
        }
        setLiveMetrics(next);
        setTraffic(nextTraffic);
      } catch {
        // Keep the last trustworthy snapshot when metrics are temporarily unavailable.
      }
    };
    refreshMetrics();
    const interval = setInterval(refreshMetrics, 5000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [apiKey]);

  const openErrors = async (name) => {
    setErrorsView({ loading: true, name });
    openDialog(WORKER_ERRORS_DIALOG_ID)();
    try {
      const { errors } = await apiJson(
        apiKey,
        `/api/admin/workers/${encodeURIComponent(name)}/errors?limit=10`,
      );
      setErrorsView({ errors, name });
    } catch (err) {
      setErrorsView({ error: err.message, name });
    }
  };

  // For each multi-version name, the highest *enabled* version serves `latest`.
  const servingVersion = new Map();
  for (const worker of data.workers) {
    if (worker.status === "disabled") continue;
    const current = servingVersion.get(worker.name);
    if (!current || compareSemver(worker.version, current) > 0) {
      servingVersion.set(worker.name, worker.version);
    }
  }

  const runtimeFor = (worker) =>
    runtimeWorkers.find(
      (entry) =>
        entry.name === worker.name &&
        entry.version === worker.version &&
        (entry.namespace || null) === (worker.namespace || null),
    );
  const healthStatusFor = (worker) => runtimeFor(worker)?.health?.status || "unobserved";
  const healthNeedsAttention = (worker) => ["degraded", "failing"].includes(healthStatusFor(worker));

  const toggle = async (worker, enable) => {
    const key = `${worker.name}@${worker.version}`;
    setBusy(key);
    try {
      const action = enable ? "enable" : "disable";
      await apiJson(
        apiKey,
        `/api/admin/workers/${encodeURIComponent(worker.name)}/${action}?version=${encodeURIComponent(worker.version)}`,
        { method: "POST" },
      );
      await onDeployed();
    } finally {
      setBusy(null);
    }
  };

  const toggleGroup = (name, defaultOpen) => {
    setOpenGroups((current) => ({
      ...current,
      [name]: !(current[name] ?? defaultOpen),
    }));
  };

  // Group the flat worker list into apps, newest version first.
  const groups = [];
  const byName = new Map();
  for (const worker of data.workers) {
    let group = byName.get(worker.name);
    if (!group) {
      group = { name: worker.name, versions: [] };
      byName.set(worker.name, group);
      groups.push(group);
    }
    group.versions.push(worker);
  }
  for (const group of groups) {
    group.versions.sort((a, b) => compareSemver(b.version, a.version));
  }

  const normalizedQuery = query.trim().toLowerCase();
  const visibleGroups = groups
    .filter((group) => {
      const kind = kindLabel(group.versions[0].kind).toLowerCase();
      const matchesQuery = !normalizedQuery || group.name.toLowerCase().includes(normalizedQuery) || group.versions.some((worker) => worker.version.toLowerCase().includes(normalizedQuery) || workerUrl(worker, false).toLowerCase().includes(normalizedQuery));
      const matchesKind = kindFilter.length === 0 || kindFilter.includes(kind);
      const routingStates = group.versions.map((worker) => worker.status === "disabled" ? "disabled" : servingVersion.get(worker.name) === worker.version ? "default" : "enabled");
      const healthStates = group.versions.map(healthStatusFor);
      const matchesRouting = routingFilter.length === 0 || routingFilter.some((status) => routingStates.includes(status));
      const matchesHealth = healthFilter.length === 0 || healthFilter.some((status) => healthStates.includes(status));
      return matchesQuery && matchesKind && matchesRouting && matchesHealth;
    })
    .sort((a, b) => sort === "name" ? a.name.localeCompare(b.name) : sort === "versions" ? b.versions.length - a.versions.length : 0);
  const enabledCount = data.workers.filter((worker) => worker.status !== "disabled").length;
  const attentionCount = groups.filter((group) => workerErrors[group.name]?.count > 0 || group.versions.some((worker) => worker.status === "disabled" || healthNeedsAttention(worker))).length;
  const kindCounts = new Map();
  for (const group of groups) {
    const kind = kindLabel(group.versions[0].kind).toLowerCase();
    kindCounts.set(kind, (kindCounts.get(kind) || 0) + 1);
  }
  const routingCounts = {
    default: servingVersion.size,
    disabled: data.workers.filter((worker) => worker.status === "disabled").length,
    enabled: Math.max(0, enabledCount - servingVersion.size),
  };
  const healthCounts = { degraded: 0, failing: 0, healthy: 0, unobserved: 0 };
  for (const worker of data.workers) {
    const status = healthStatusFor(worker);
    healthCounts[status] = (healthCounts[status] || 0) + 1;
  }
  const isFiltered = Boolean(normalizedQuery) || kindFilter.length > 0 || routingFilter.length > 0 || healthFilter.length > 0;
  const pageCount = Math.max(1, Math.ceil(visibleGroups.length / pageSize));
  const safePageIndex = Math.min(pageIndex, pageCount - 1);
  const pageStart = safePageIndex * pageSize;
  const paginatedGroups = visibleGroups.slice(pageStart, pageStart + pageSize);
  const resetFilters = () => {
    setQuery("");
    setKindFilter([]);
    setRoutingFilter([]);
    setHealthFilter([]);
  };

  useEffect(() => {
    setPageIndex(0);
  }, [query, kindFilter, routingFilter, healthFilter, sort, pageSize]);

  return html`
    <div>
      <${DeployDialog} apiKey=${apiKey} onDeployed=${onDeployed} />
      <div class="mb-5 flex flex-col gap-4">
        <div class="border-border bg-card flex flex-col gap-4 rounded-xl border p-4 shadow-xs lg:flex-row lg:items-center lg:justify-between">
          <div class="flex items-center gap-3">
            <span class="bg-primary/10 text-primary flex size-11 items-center justify-center rounded-xl"><${Icon} icon="lucide:boxes" size="20" /></span>
            <div>
              <h2 class="text-2xl font-semibold tracking-tight">Apps</h2>
              <p class="text-muted-foreground text-sm">Versioned runtime inventory</p>
            </div>
          </div>
          <div class="grid w-full grid-cols-3 divide-x lg:w-auto">
            <div class="px-2 text-center sm:px-5"><strong class="block text-2xl">${data.workers.length}</strong><span class="text-muted-foreground text-xs sm:text-sm">Versions live</span></div>
            <div class="px-2 text-center sm:px-5"><strong class="block text-2xl text-emerald-700">${enabledCount}</strong><span class="text-muted-foreground text-xs sm:text-sm">Routable</span></div>
            <div class="px-2 text-center sm:px-5"><strong class=${`block text-2xl ${attentionCount ? "text-amber-700" : ""}`}>${attentionCount}</strong><span class="text-muted-foreground text-xs sm:text-sm">Need attention</span></div>
          </div>
        </div>
        <p class="text-muted-foreground text-sm">Every app serves each version at its own pathname — latest is selected from enabled versions.</p>
        <div aria-label="Filter apps" class="flex flex-col gap-2 rounded-lg" role="toolbar">
          <div class="flex flex-wrap items-center gap-2">
          <${InputGroup} className="w-full sm:w-72">
            <${InputGroupAddon}><${Icon} icon="lucide:search" size="15" /><//>
            <${InputGroupInput} aria-label="Search apps" onInput=${(event) => setQuery(event.currentTarget.value)} placeholder="Search apps, versions, pathnames…" value=${query} />
          <//>
          <${Select} className="shrink-0" multiple onValueChange=${setKindFilter} value=${kindFilter}>
            <${SelectTrigger} className="border-dashed font-normal">${kindFilter.length ? html`<button aria-label="Clear type filter" class="-m-1 flex size-6 items-center justify-center rounded-sm hover:bg-accent" onClick=${(event) => { event.preventDefault(); event.stopPropagation(); setKindFilter([]); }} type="button"><${Icon} icon="lucide:circle-x" size="15" /></button>` : html`<${Icon} icon="lucide:circle-plus" size="15" />`}<${SelectValue}>${kindFilter.length === 0 ? "Type" : kindFilter.length === 1 ? kindFilterLabel(kindFilter[0]) : `${kindFilter.length} types`}<//><//>
            <${SelectContent}>
              <${SelectGroup}>
                ${[...kindCounts.entries()].map(([kind, count]) => html`<${SelectItem} value=${kind}>${kindFilterLabel(kind)} (${count})<//>`)}
              <//>
            <//>
          <//>
          <${Select} className="shrink-0" multiple onValueChange=${setRoutingFilter} value=${routingFilter}>
            <${SelectTrigger} className="border-dashed font-normal">${routingFilter.length ? html`<button aria-label="Clear routing filter" class="-m-1 flex size-6 items-center justify-center rounded-sm hover:bg-accent" onClick=${(event) => { event.preventDefault(); event.stopPropagation(); setRoutingFilter([]); }} type="button"><${Icon} icon="lucide:circle-x" size="15" /></button>` : html`<${Icon} icon="lucide:circle-plus" size="15" />`}<${SelectValue}>${routingFilter.length === 0 ? "Routing" : routingFilter.length === 1 ? routingLabel(routingFilter[0]) : `${routingFilter.length} states`}<//><//>
            <${SelectContent}>
              <${SelectGroup}>
                <${SelectItem} value="default">Default (${routingCounts.default})<//>
                <${SelectItem} value="enabled">Enabled (${routingCounts.enabled})<//>
                <${SelectItem} value="disabled">Disabled (${routingCounts.disabled})<//>
              <//>
            <//>
          <//>
          <${Select} className="shrink-0" multiple onValueChange=${setHealthFilter} value=${healthFilter}>
            <${SelectTrigger} className="border-dashed font-normal">${healthFilter.length ? html`<button aria-label="Clear health filter" class="-m-1 flex size-6 items-center justify-center rounded-sm hover:bg-accent" onClick=${(event) => { event.preventDefault(); event.stopPropagation(); setHealthFilter([]); }} type="button"><${Icon} icon="lucide:circle-x" size="15" /></button>` : html`<${Icon} icon="lucide:circle-plus" size="15" />`}<${SelectValue}>${healthFilter.length === 0 ? "Health" : healthFilter.length === 1 ? healthPresentation(healthFilter[0]).label : `${healthFilter.length} states`}<//><//>
            <${SelectContent}>
              <${SelectGroup}>
                ${["unobserved", "healthy", "degraded", "failing"].map((status) => html`<${SelectItem} value=${status}>${healthPresentation(status).label} (${healthCounts[status] || 0})<//>`)}
              <//>
            <//>
          <//>
          ${isFiltered && html`<${Button} aria-label="Clear filters" className="border-dashed" onClick=${resetFilters} size="sm" type="button" variant="outline"><${Icon} icon="lucide:x" size="15" /> Clear<//>`}
          <div class="flex w-full items-center justify-between gap-2 sm:ml-auto sm:w-auto sm:justify-end">
            <span class="text-muted-foreground text-sm">${visibleGroups.length} of ${groups.length} apps</span>
            <${Select} onValueChange=${setSort} value=${sort}>
              <${SelectTrigger} aria-label="Sort apps"><${Icon} icon="lucide:arrow-up-down" size="15" /><${SelectValue}>${sort === "recent" ? "Runtime order" : sort === "name" ? "Name A–Z" : "Most versions"}<//><//>
              <${SelectContent} align="end">
                <${SelectGroup}>
                  <${SelectItem} value="recent">Runtime order<//>
                  <${SelectItem} value="name">Name A–Z<//>
                  <${SelectItem} value="versions">Most versions<//>
                <//>
              <//>
            <//>
          </div>
          </div>
        </div>
      </div>
      ${visibleGroups.length === 0 && html`<div class="border-border bg-muted/20 rounded-xl border border-dashed p-10 text-center"><p class="font-medium">No apps match this view.</p><p class="text-muted-foreground mt-1 text-sm">Clear the search or select another filter.</p></div>`}
      <div class="grid gap-3">
        ${paginatedGroups.map((group) => {
          const serving = servingVersion.get(group.name);
          const kind = kindLabel(group.versions[0].kind);
          const scope = group.versions[0].namespace;
          const errorInfo = workerErrors[group.name];
          const disabledVersions = group.versions.filter((worker) => worker.status === "disabled");
          const unhealthyVersions = group.versions
            .filter(healthNeedsAttention)
            .map((worker) => ({ ...worker, health: { status: healthStatusFor(worker) } }));
          const hasAttention = errorInfo?.count > 0 || disabledVersions.length > 0 || unhealthyVersions.length > 0;
          const enabledVersions = group.versions.filter((worker) => worker.status !== "disabled").length;
          const isControlPlane = group.name === "cpanel";
          const defaultOpen = group.versions.length > 1 || hasAttention;
          const isOpen = openGroups[group.name] ?? defaultOpen;
          return html`
            <${Collapsible} className="border-border bg-card rounded-xl border" key=${group.name} open=${isOpen}>
              <${CollapsibleTrigger} className="flex w-full items-center gap-2 px-3 py-3 text-left sm:gap-3 sm:px-4" interactive=${false} onClick=${(event) => event.preventDefault()}>
                <span
                  aria-expanded=${isOpen}
                  aria-label=${`${isOpen ? "Collapse" : "Expand"} ${group.name}`}
                  class="hover:bg-muted focus-visible:ring-ring flex size-7 cursor-pointer items-center justify-center rounded-md outline-none focus-visible:ring-2"
                  onClick=${(event) => {
                    event.preventDefault();
                    event.stopPropagation();
                    toggleGroup(group.name, defaultOpen);
                  }}
                  onKeyDown=${(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      event.stopPropagation();
                      toggleGroup(group.name, defaultOpen);
                    }
                  }}
                  role="button"
                  tabIndex="0"
                >
                  <${Icon} className=${`text-muted-foreground transition-transform ${isOpen ? "rotate-90" : ""}`} icon="lucide:chevron-right" size="16" />
                </span>
                <${Tooltip} align="start" content=${`Application type: ${kindFilterLabel(kind.toLowerCase())}`}>
                  <span aria-label=${`Application type: ${kindFilterLabel(kind.toLowerCase())}`} class="bg-primary/10 text-primary flex size-9 shrink-0 items-center justify-center rounded-lg" role="img" tabIndex="0"><${Icon} icon=${kindIcon(kind)} size="18" /></span>
                <//>
                <div class="min-w-0 flex-1">
                  <div class="flex flex-wrap items-center gap-2">
                    <span class="min-w-0 truncate font-semibold">${group.name}</span>
                    ${serving && html`<${Badge} title="Default version" variant="secondary"><span class="font-mono">${serving}</span><//>`}
                    ${hasAttention && html`<${AttentionPopover}
                      disabledVersions=${disabledVersions}
                      errorInfo=${errorInfo}
                      group=${group}
                      onOpenLogs=${onViewLogs}
                      onOpenObservability=${onViewObservability}
                      onViewAll=${onViewAllLogs}
                      unhealthyVersions=${unhealthyVersions}
                    />`}
                    ${isControlPlane && html`<${Badge} variant="outline">Control plane<//>`}
                  </div>
                  <p class="text-muted-foreground mt-1 truncate text-xs">
                    ${scope ? html`<span class="font-mono">${scope}</span> · ` : ""}${group.versions.length} version${group.versions.length > 1 ? "s" : ""} · uploaded
                  </p>
                </div>
                <div class="ml-auto flex shrink-0 items-center gap-2">
                  <${Button} aria-label=${`Deploy a new version of ${group.name}`} onClick=${(event) => { event.preventDefault(); openDialog(DEPLOY_DIALOG_ID)(); }} size="sm" type="button" variant="outline"><${Icon} icon="lucide:plus" size="14" /><span class="hidden sm:inline">New version</span><//>
                </div>
              <//>
              <${CollapsibleContent}>
              <div class="text-muted-foreground bg-muted/30 border-border/70 hidden grid-cols-[70px_minmax(150px,1fr)_120px_144px_120px_130px_120px] gap-3 border-t px-4 py-2 text-[11px] font-semibold tracking-wide uppercase lg:grid">
                <span>Version</span><span>Pathname</span><span>Routing</span><span>Health</span><span>Processes</span><span>Traffic</span><span></span>
              </div>
              ${group.versions.map((worker) => {
                const isLatest = serving === worker.version;
                const disabled = worker.status === "disabled";
                const url = workerUrl(worker, isLatest);
                const rowKey = `${worker.name}@${worker.version}`;
                const runtime = runtimeFor(worker);
                const health = healthPresentation(runtime?.health?.status || "unobserved");
                const trafficKey = `${worker.namespace || ""}/${worker.name}@${worker.version}`;
                const trafficInfo = traffic[trafficKey] || { p95: runtime?.requestDurationMsP95 || null, rate: null, total: runtime?.requestTotal ?? runtime?.requests ?? 0 };
                const canDisable = !isControlPlane || enabledVersions > 1;
                return html`
                  <div class="border-border/60 relative grid min-w-0 grid-cols-1 gap-3 border-t px-3 py-3 sm:grid-cols-2 sm:px-4 lg:grid-cols-[70px_minmax(150px,1fr)_120px_144px_120px_130px_120px] lg:items-center lg:gap-3" key=${rowKey}>
                    <div class="min-w-0 pr-36 lg:pr-0">
                      <span class="text-muted-foreground mb-1 block text-[11px] font-semibold tracking-wide uppercase lg:hidden">Version</span>
                      <span class="flex items-center gap-1">
                      <${Badge} tabIndex="0" title=${isLatest ? "Default version" : disabled ? "Disabled version" : "Enabled version"} variant="secondary">
                        <span class=${`font-mono ${disabled ? "text-muted-foreground" : ""}`}>${worker.version}</span>
                      <//>
                      </span>
                    </div>
                    <div class="min-w-0 sm:col-span-2 lg:col-span-1">
                      <span class="text-muted-foreground mb-1 block text-[11px] font-semibold tracking-wide uppercase lg:hidden">Pathname</span>
                      <span class=${`block max-w-full truncate font-mono text-sm text-foreground/80 ${disabled ? "line-through opacity-60" : ""}`} title=${url}>${url}</span>
                    </div>
                    <div class="min-w-0">
                      <span class="text-muted-foreground mb-1 block text-[11px] font-semibold tracking-wide uppercase lg:hidden">Routing</span>
                      <span class="flex items-center gap-1 text-sm">
                      ${disabled
                        ? html`<span class="text-muted-foreground flex items-center gap-1.5"><span class="bg-muted-foreground/40 size-2 rounded-full"></span>Disabled</span>`
                        : isLatest
                          ? html`<span class="flex items-center gap-1.5 font-medium text-emerald-700"><span class="size-2 rounded-full bg-emerald-500 ring-2 ring-emerald-100"></span>Default</span>`
                          : html`<span class="text-primary flex items-center gap-1.5"><span class="bg-primary size-2 rounded-full"></span>Enabled</span>`}
                      <${HelpTooltip}
                        align="start"
                        content=${isLatest ? "Default version: receives requests sent to the unversioned pathname." : disabled ? "Disabled version: remains listed, but its versioned pathname does not serve requests." : "Enabled version: serves its versioned pathname but is not the default."}
                      />
                      </span>
                    </div>
                    <div class="min-w-0">
                      <span class="text-muted-foreground mb-1 block text-[11px] font-semibold tracking-wide uppercase lg:hidden">Health</span>
                      <span class=${`flex items-center gap-1 text-sm ${health.className}`}>
                        <span class=${`size-2 rounded-full ${health.dot}`}></span>
                        <span>${health.label}</span>
                        <${HelpTooltip} align="start" content=${healthHelp(runtime?.health)} />
                      </span>
                    </div>
                    <div class="min-w-0 text-sm">
                      <span class="text-muted-foreground mb-1 block text-[11px] font-semibold tracking-wide uppercase lg:hidden">Processes</span>
                      ${runtime?.maxProcesses ? html`<div class="flex items-center gap-2"><${ProcessCapacity} runtime=${runtime} /><div class="flex items-center gap-1"><span class="font-mono font-medium">${(runtime.activeProcesses || 0) + (runtime.idleProcesses || 0) + (runtime.terminatingProcesses || 0)}/${runtime.maxProcesses}</span><span class="text-muted-foreground">${runtime.activeProcesses ? "Active" : runtime.idleProcesses ? "Idle" : runtime.terminatingProcesses ? "Terminating" : "Cold"}</span></div></div>${(runtime.queued || runtime.rejectedTotal || runtime.timeoutTotal) ? html`<p class="mt-1 text-xs text-amber-700">Q ${runtime.queued || 0} · Rej ${runtime.rejectedTotal || 0} · Timeout ${runtime.timeoutTotal || 0}</p>` : ""}` : html`<span class="text-muted-foreground">Cold</span>`}
                    </div>
                    <div class="min-w-0 sm:col-span-2 lg:col-span-1">
                      <span class="text-muted-foreground mb-1 block text-[11px] font-semibold tracking-wide uppercase lg:hidden">Traffic</span>
                      ${trafficInfo.total > 0
                        ? html`<div class="flex items-center gap-1"><p class="font-medium">${trafficInfo.rate == null ? "Observing" : `${trafficInfo.rate.toFixed(1)} req/min`}</p><${HelpTooltip} align="end" content="Request rate uses a rolling 60-second window. p95 is the duration below which 95% of the latest requests completed. Total is cumulative since the runtime started." /></div><p class="text-muted-foreground text-xs">${trafficInfo.p95 == null ? "P95 collecting" : `P95 ${trafficInfo.p95}ms`} · ${trafficInfo.total} total</p>`
                        : html`<div class="text-muted-foreground flex items-center gap-1"><span>No traffic yet</span><${HelpTooltip} align="end" content="No requests have reached this worker version since the runtime started." /></div>`}
                    </div>
                    <div class="absolute top-2 right-2 flex items-center justify-self-end lg:static">
                      <${Tooltip} align="end" content="Browse this version's files">
                        <${Button} aria-label=${`Browse files for ${url}`} onClick=${() => onViewFiles(worker, isLatest)} size="icon-sm" type="button" variant="ghost"><${Icon} icon="lucide:folder-open" size="14" /><//>
                      <//>
                      <${Tooltip} align="end" content="Open public URL in a new tab">
                        <${Button} aria-label=${`Open URL ${url} in a new tab`} onClick=${() => window.open(url, "_blank", "noopener")} size="icon-sm" type="button" variant="ghost"><${Icon} icon="lucide:external-link" size="14" /><//>
                      <//>
                      <${DropdownMenu}>
                        <${DropdownMenuTrigger} className="text-muted-foreground" size="icon" variant="ghost"><${Icon} icon="lucide:ellipsis-vertical" size="16" /><//>
                        <${DropdownMenuContent} align="end">
                        <${DropdownMenuItem} onClick=${() => onViewLogs(worker)}><${Icon} icon="lucide:scroll-text" size="15" /> View logs<//>
                        ${disabled
                          ? html`<${DropdownMenuItem} disabled=${busy === rowKey} onClick=${() => toggle(worker, true)}><${Icon} icon="lucide:rotate-ccw" size="15" /> Enable version<//>`
                          : canDisable
                            ? html`<${DropdownMenuItem} disabled=${busy === rowKey} onClick=${() => toggle(worker, false)}><${Icon} icon="lucide:power-off" size="15" /> Disable version<//>`
                            : html`<${DropdownMenuItem} disabled><${Icon} icon="lucide:shield-check" size="15" /> Default version required<//>`}
                        ${errorInfo &&
                        errorInfo.count > 0 &&
                        html`<${DropdownMenuItem} className="text-destructive" onClick=${() => openErrors(worker.name)}><${Icon} icon="lucide:triangle-alert" size="15" /> View errors (${errorInfo.count})<//>`}
                        <//>
                      <//>
                    </div>
                  </div>
                `;
              })}
              <//>
            <//>
          `;
        })}
      </div>
      ${visibleGroups.length > 0 && html`
        <div class="mt-4 flex flex-col items-start justify-between gap-3 px-1 sm:flex-row sm:flex-wrap sm:items-center">
          <p class="text-muted-foreground text-sm">Showing ${pageStart + 1}–${Math.min(pageStart + pageSize, visibleGroups.length)} of ${visibleGroups.length} apps</p>
          <div class="flex w-full flex-wrap items-center justify-between gap-3 sm:w-auto sm:justify-end">
            <span class="text-sm font-medium">Page ${safePageIndex + 1} of ${pageCount}</span>
            <div class="flex items-center gap-1">
              <${Button} aria-label="First page" disabled=${safePageIndex === 0} onClick=${() => setPageIndex(0)} size="icon-sm" type="button" variant="outline"><${Icon} icon="lucide:chevrons-left" size="15" /><//>
              <${Button} aria-label="Previous page" disabled=${safePageIndex === 0} onClick=${() => setPageIndex((value) => Math.max(0, value - 1))} size="icon-sm" type="button" variant="outline"><${Icon} icon="lucide:chevron-left" size="15" /><//>
              <${Button} aria-label="Next page" disabled=${safePageIndex >= pageCount - 1} onClick=${() => setPageIndex((value) => Math.min(pageCount - 1, value + 1))} size="icon-sm" type="button" variant="outline"><${Icon} icon="lucide:chevron-right" size="15" /><//>
              <${Button} aria-label="Last page" disabled=${safePageIndex >= pageCount - 1} onClick=${() => setPageIndex(pageCount - 1)} size="icon-sm" type="button" variant="outline"><${Icon} icon="lucide:chevrons-right" size="15" /><//>
            </div>
            <${Select} onValueChange=${(value) => setPageSize(Number(value))} value=${String(pageSize)}>
              <${SelectTrigger} aria-label="Apps per page"><${SelectValue}>${pageSize} / page<//><//>
              <${SelectContent} align="end">
                <${SelectGroup}>
                  ${[10, 25, 50, 100].map((size) => html`<${SelectItem} value=${String(size)}>${size} per page<//>`)}
                <//>
              <//>
            <//>
          </div>
        </div>
      `}
      <${Dialog} id=${WORKER_ERRORS_DIALOG_ID} className="max-w-xl">
        <${DialogContent} className="max-w-xl">
          <${DialogHeader}>
            <${DialogTitle}>Recent errors — ${errorsView?.name || ""}<//>
            <${DialogDescription}>Latest worker dispatch failures, newest first.<//>
          <//>
          ${errorsView?.loading && html`<${Spinner} size="16" />`}
          ${errorsView?.error &&
          html`<${Alert} variant="destructive"><${AlertTitle}>Could not load errors<//><${AlertDescription}>${errorsView.error}<//><//>`}
          ${errorsView?.errors &&
          (errorsView.errors.length
            ? html`
                <div class="grid max-h-80 gap-2 overflow-y-auto">
                  ${errorsView.errors.map(
                    (entry, index) => html`
                      <div class="border-border rounded-md border p-2 text-xs" key=${index}>
                        <div class="flex items-center gap-2">
                          <${Badge} variant="destructive">${entry.status}<//>
                          <span class="font-mono font-medium">${entry.code}</span>
                          <span class="text-muted-foreground ml-auto font-mono">${entry.requestId}</span>
                        </div>
                        <p class="mt-1 break-words">${entry.message}</p>
                      </div>
                    `,
                  )}
                </div>
              `
            : html`<p class="text-muted-foreground text-sm">No errors recorded.</p>`)}
          <div class="flex justify-end">
            <${Button} onClick=${closeDialog(WORKER_ERRORS_DIALOG_ID)} size="sm" type="button" variant="outline">Close<//>
          </div>
        <//>
      <//>
    </div>
  `;
}

const KIND_BY_MANIFEST = {
  backend: "RoutesTable",
  fetch: "FetchHandler",
  fullstack: "Fullstack",
  routes: "RoutesTable",
  serverless: "FetchHandler",
  spa: "StaticSpa",
  ssr: "Fullstack",
  static: "StaticSpa",
  wasm: "WasmModule",
};

// Minimal manifest.yaml reader for the pre-install preview: top-level
// `key: value` pairs only. The server-side install revalidates everything.
function parseManifestYaml(text) {
  const fields = {};
  for (const line of text.split("\n")) {
    const match = line.match(/^([A-Za-z][A-Za-z0-9_-]*):\s*(.+?)\s*$/);
    if (match) {
      fields[match[1]] = match[2].replace(/^["']|["']$/g, "");
    }
  }
  return fields;
}

function inferKind(fields, entrypoint) {
  if (fields.kind && KIND_BY_MANIFEST[fields.kind.toLowerCase()]) {
    return KIND_BY_MANIFEST[fields.kind.toLowerCase()];
  }
  if (entrypoint?.endsWith(".html")) return "StaticSpa";
  if (entrypoint?.endsWith(".wasm") || entrypoint?.endsWith(".wat")) return "WasmModule";
  return "FetchHandler";
}

function previewPackage(zipBytes) {
  const files = unzipSync(zipBytes);
  let paths = Object.keys(files).filter((path) => !path.endsWith("/"));
  if (!paths.length) {
    throw new Error("Zip is empty.");
  }
  // Folder zips nest everything under one top-level dir; the runtime unwraps it.
  const topLevel = new Set(paths.map((path) => path.split("/")[0]));
  let prefix = "";
  if (topLevel.size === 1 && paths.every((path) => path.includes("/"))) {
    prefix = `${[...topLevel][0]}/`;
    paths = paths.map((path) => path.slice(prefix.length));
  }
  const read = (name) =>
    paths.includes(name) ? new TextDecoder().decode(files[prefix + name]) : null;

  let fields = {};
  const manifestText = read("manifest.yaml") ?? read("manifest.yml");
  if (manifestText) {
    fields = parseManifestYaml(manifestText);
  } else {
    const packageText = read("package.json");
    if (packageText) {
      try {
        const parsed = JSON.parse(packageText);
        fields = { entrypoint: parsed.module || parsed.main, name: parsed.name, version: parsed.version };
      } catch {
        // fall through to the missing-manifest error below
      }
    }
  }
  const entrypoint =
    fields.entrypoint ||
    ["index.html", "index.ts", "index.js", "index.mjs", "index.wasm", "index.wat"].find((candidate) =>
      paths.includes(candidate),
    );
  if (!fields.name) {
    throw new Error("Package must include manifest.yaml (or package.json) with a worker name.");
  }
  if (!entrypoint) {
    throw new Error("Package has no entrypoint (manifest entrypoint or index.*).");
  }
  return {
    entrypoint,
    fileCount: paths.length,
    files: paths.slice(0, 8),
    kind: inferKind(fields, entrypoint),
    name: fields.name,
    version: fields.version || "latest",
    visibility: fields.visibility || "protected",
  };
}

async function collectDroppedFiles(dataTransfer) {
  const entries = [...dataTransfer.items]
    .map((item) => item.webkitGetAsEntry?.())
    .filter(Boolean);
  const collected = {};
  async function walk(entry, path) {
    if (entry.isFile) {
      const file = await new Promise((resolve, reject) => entry.file(resolve, reject));
      collected[path + entry.name] = new Uint8Array(await file.arrayBuffer());
      return;
    }
    const reader = entry.createReader();
    const children = await new Promise((resolve, reject) => reader.readEntries(resolve, reject));
    for (const child of children) {
      await walk(child, `${path + entry.name}/`);
    }
  }
  for (const entry of entries) {
    await walk(entry, "");
  }
  return collected;
}

function DeployDialog({ apiKey, onDeployed }) {
  const [stage, setStage] = useState({ kind: "idle" });
  const [dragging, setDragging] = useState(false);
  const zipInput = useRef(null);
  const folderInput = useRef(null);

  const reset = () => setStage({ kind: "idle" });

  const stagePackage = (zipBytes) => {
    try {
      setStage({ kind: "preview", preview: previewPackage(zipBytes), zipBytes });
    } catch (err) {
      setStage({ error: err.message, kind: "error" });
    }
  };

  const stageFileMap = (fileMap) => {
    if (!Object.keys(fileMap).length) {
      setStage({ error: "Nothing to deploy in the dropped selection.", kind: "error" });
      return;
    }
    stagePackage(zipSync(fileMap));
  };

  const handleDrop = async (event) => {
    event.preventDefault();
    setDragging(false);
    const files = [...event.dataTransfer.files];
    if (files.length === 1 && files[0].name.endsWith(".zip")) {
      stagePackage(new Uint8Array(await files[0].arrayBuffer()));
      return;
    }
    stageFileMap(await collectDroppedFiles(event.dataTransfer));
  };

  const handleZipPick = async (event) => {
    const [file] = event.currentTarget.files;
    if (file) stagePackage(new Uint8Array(await file.arrayBuffer()));
    event.currentTarget.value = "";
  };

  const handleFolderPick = async (event) => {
    const files = [...event.currentTarget.files];
    const fileMap = {};
    for (const file of files) {
      fileMap[file.webkitRelativePath || file.name] = new Uint8Array(await file.arrayBuffer());
    }
    stageFileMap(fileMap);
    event.currentTarget.value = "";
  };

  const install = async () => {
    if (stage.kind !== "preview") return;
    setStage({ ...stage, installing: true });
    try {
      const installed = await apiJson(apiKey, "/api/admin/workers/install", {
        body: stage.zipBytes,
        headers: { "content-type": "application/zip" },
        method: "POST",
      });
      setStage({ installed, kind: "success" });
      await onDeployed();
    } catch (err) {
      setStage({ error: err.message, kind: "error" });
    }
  };

  return html`
    <${Dialog} id=${DEPLOY_DIALOG_ID} className="max-w-lg">
      <${DialogContent} className="max-w-lg">
        <${DialogHeader}>
          <${DialogTitle}>Deploy an app<//>
          <${DialogDescription}>
            Drop a zip or a folder — the app goes live on this runtime, no restart. Limit: 4 MiB.
          <//>
        <//>
        <div
          class=${`flex min-h-44 cursor-pointer flex-col items-center justify-center gap-2 rounded-lg border-2 border-dashed p-6 text-center transition-colors ${
            dragging ? "border-primary bg-primary/5" : "border-border"
          }`}
          data-testid="deploy-dropzone"
          onClick=${() => zipInput.current?.click()}
          onDragLeave=${() => setDragging(false)}
          onDragOver=${(event) => {
            event.preventDefault();
            setDragging(true);
          }}
          onDrop=${handleDrop}
        >
          <${Icon} className="text-muted-foreground" icon="lucide:upload-cloud" size="32" />
          <p class="text-sm font-medium">Drag and drop a zip or app folder here</p>
          <p class="text-muted-foreground text-xs">
            The package needs a manifest.yaml (or package.json) with a name, and an entrypoint.
          </p>
          <div class="mt-2 flex gap-2">
            <${Button} onClick=${(event) => { event.stopPropagation(); zipInput.current?.click(); }} size="sm" type="button" variant="outline">
              <${Icon} icon="lucide:file-archive" size="14" /> Choose zip
            <//>
            <${Button} onClick=${(event) => { event.stopPropagation(); folderInput.current?.click(); }} size="sm" type="button" variant="outline">
              <${Icon} icon="lucide:folder-open" size="14" /> Choose folder
            <//>
          </div>
          <input accept=".zip" class="hidden" onChange=${handleZipPick} ref=${zipInput} type="file" />
          <input class="hidden" onChange=${handleFolderPick} ref=${folderInput} type="file" webkitdirectory />
        </div>

        ${stage.kind === "error" &&
        html`
          <${Alert} className="mt-4" variant="destructive">
            <${AlertTitle}>Package rejected<//>
            <${AlertDescription}>${stage.error}<//>
          <//>
        `}

        ${stage.kind === "preview" &&
        html`
          <div class="border-border mt-4 rounded-lg border p-4" data-testid="deploy-preview">
            <div class="flex items-center justify-between gap-4">
              <div class="min-w-0">
                <p class="text-sm font-semibold">
                  <code class="bg-muted rounded px-1.5 py-0.5 text-xs">${stage.preview.name}</code>
                  <span class="text-muted-foreground ml-2 text-xs">v${stage.preview.version}</span>
                </p>
                <p class="text-muted-foreground mt-1 text-xs">
                  ${stage.preview.kind} · entrypoint ${stage.preview.entrypoint} ·
                  ${" "}${stage.preview.fileCount} file(s) · ${stage.preview.visibility}
                </p>
              </div>
              <div class="flex shrink-0 gap-2">
                <${Button} onClick=${() => setStage({ kind: "idle" })} size="sm" type="button" variant="ghost">Cancel<//>
                <${Button} disabled=${stage.installing} onClick=${install} size="sm" type="button">
                  ${stage.installing ? html`<${Spinner} size="14" />` : html`<${Icon} icon="lucide:rocket" size="14" />`}
                  Deploy
                <//>
              </div>
            </div>
            <p class="text-muted-foreground mt-2 truncate text-xs">${stage.preview.files.join(" · ")}</p>
          </div>
        `}

        ${stage.kind === "success" &&
        html`
          <${Alert} className="mt-4" data-testid="deploy-success">
            <${AlertTitle}>
              ${stage.installed.name}@${stage.installed.version} is live
            <//>
            <${AlertDescription}>
              <span class="flex flex-wrap items-center gap-2 pt-1">
                <a class="text-primary font-medium underline underline-offset-2" href=${stage.installed.url} rel="noreferrer" target="_blank">
                  ${stage.installed.url}
                </a>
                <${Badge} variant="outline">${stage.installed.kind}<//>
                <${Badge} variant=${stage.installed.visibility === "public" ? "default" : "secondary"}>
                  ${stage.installed.visibility}
                <//>
                ${stage.installed.authRequired &&
                html`<span class="text-muted-foreground text-xs">requests need an API key</span>`}
                <${Button} onClick=${reset} size="sm" type="button" variant="outline">Deploy another<//>
              </span>
              <p class="text-muted-foreground mt-2 text-xs">
                Health and errors show up in the Workers list below.
              </p>
            <//>
          <//>
        `}

        <div class="mt-2 flex justify-end">
          <${Button} onClick=${() => { reset(); closeDialog(DEPLOY_DIALOG_ID)(); }} size="sm" type="button" variant="outline">Close<//>
        </div>
      <//>
    <//>
  `;
}

function Shell({ apiKey, data, onLogout, onRefresh, refreshing }) {
  const initialRoute = readCpanelRoute();
  const [view, setView] = useState(initialRoute.view);
  const [filesTarget, setFilesTarget] = useState(initialRoute.target || null);
  const [filesPath, setFilesPath] = useState(initialRoute.path || "");
  const [filesReload, setFilesReload] = useState(0);
  const [viewReload, setViewReload] = useState(0);
  const [filesStatus, setFilesStatus] = useState(null);
  const filesInput = useRef(null);
  const folderInput = useRef(null);
  const active = VIEWS.find((entry) => entry.id === view)
    || (view === "observability-global" ? VIEWS.find((entry) => entry.id === "observability-overview") : null)
    || VIEWS[0];
  const principal = data.principal;

  const applyRoute = (route) => {
    setView(route.view);
    setFilesTarget(route.target || null);
    setFilesPath(route.path || "");
    setFilesStatus(null);
  };
  const navigate = (route, replace = false) => {
    applyRoute(route);
    history[replace ? "replaceState" : "pushState"](null, "", cpanelLocation(route.view, route.target, route.path));
  };
  const navigateView = (nextView) => navigate({ view: nextView });
  const navigateFilesPath = (path) => navigate({ path, target: filesTarget, view: "files" });
  const openFiles = (worker, isLatest) => {
    const target = { name: worker.name, url: workerUrl(worker, isLatest), version: worker.version };
    navigate({ path: "", target, view: "files" });
  };
  const openLogs = (worker) => {
    const target = { name: worker.name, url: workerUrl(worker, false), version: worker.version };
    navigate({ target, view: "logs" });
  };
  const openObservability = (worker) => {
    const target = { name: worker.name, url: workerUrl(worker, false), version: worker.version };
    navigate({ target, view: "observability" });
  };

  useEffect(() => {
    const handlePopState = () => applyRoute(readCpanelRoute());
    window.addEventListener("popstate", handlePopState);
    return () => window.removeEventListener("popstate", handlePopState);
  }, []);
  const isFiles = Boolean(view === "files" && filesTarget);
  const isLogs = Boolean(view === "logs" && filesTarget);
  const isObservability = Boolean(view === "observability" && filesTarget);
  const isObservabilityOverview = view === "observability-overview";
  const isGlobalObservability = view === "observability-global";
  const isWorkerDetail = Boolean((isFiles || isLogs || isObservability) && filesTarget);
  const headerTitle = isWorkerDetail ? filesTarget.name : active.title;
  const headerDescription = isWorkerDetail ? `Version ${filesTarget.version} · ${isFiles ? "files" : isLogs ? "logs" : "observability"}` : active.description;
  const refreshesCurrentView = isFiles || isLogs || isObservability || isObservabilityOverview || isGlobalObservability;
  const refreshCurrentView = () => {
    if (isFiles) setFilesReload((value) => value + 1);
    else if (refreshesCurrentView) setViewReload((value) => value + 1);
    else onRefresh();
  };

  const uploadZip = async (zipBytes) => {
    if (!filesTarget) return;
    setFilesStatus({ uploading: true });
    try {
      const query = new URLSearchParams({ path: filesPath, version: filesTarget.version });
      await apiJson(apiKey, `/api/admin/workers/${encodeURIComponent(filesTarget.name)}/files?${query}`, {
        body: zipBytes,
        headers: { "content-type": "application/zip" },
        method: "POST",
      });
      setFilesStatus(null);
      setFilesReload((value) => value + 1);
    } catch (err) {
      setFilesStatus({ error: err.message });
    }
  };
  const handlePick = async (event) => {
    const fileMap = {};
    for (const file of [...event.currentTarget.files]) {
      fileMap[file.webkitRelativePath || file.name] = new Uint8Array(await file.arrayBuffer());
    }
    if (Object.keys(fileMap).length) uploadZip(zipSync(fileMap));
    event.currentTarget.value = "";
  };

  return html`
    <div class="flex h-full w-full overflow-hidden">
      <${Sidebar} className="responsive-sidebar">
        <${SidebarHeader}>
          <div class="flex items-center gap-2 px-2 py-1.5">
            <${Icon} className="text-foreground shrink-0" icon="lucide:cpu" size="20" />
            <span class="grid leading-tight">
              <strong class="text-sm font-semibold">EdgeR</strong>
              <small class="text-muted-foreground text-xs">cPanel</small>
            </span>
          </div>
        <//>
        <${SidebarContent}>
          <${SidebarMenu}>
            ${VIEWS.map(
              (entry) => html`
                <${SidebarMenuItem} key=${entry.id}>
                  <${SidebarMenuButton} isActive=${view === entry.id || (isGlobalObservability && entry.id === "observability-overview") || (isWorkerDetail && entry.id === "workers")} onClick=${() => navigateView(entry.id)} title=${entry.title}>
                    <${Icon} icon=${entry.icon} size="16" />
                    <span>${entry.title}</span>
                  <//>
                <//>
              `,
            )}
          <//>
        <//>
        <${SidebarFooter}>
          <div class="flex items-center gap-2 px-2 py-1.5">
            <span class="bg-muted text-muted-foreground flex size-8 items-center justify-center rounded-full text-xs font-semibold">
              ${(principal?.name || "?").slice(0, 2)}
            </span>
            <span class="grid min-w-0 flex-1 leading-tight">
              <strong class="truncate text-xs font-medium">${principal?.name || "-"}</strong>
              <small class="text-muted-foreground truncate text-xs">${principal?.role || "-"}</small>
            </span>
            <${Button} onClick=${onLogout} size="icon-sm" title="Disconnect" type="button" variant="ghost">
              <${Icon} icon="lucide:log-out" size="16" />
              <span class="sr-only">Disconnect</span>
            <//>
          </div>
        <//>
      <//>
      <${SidebarInset} className="min-w-0">
        <header class="border-border flex flex-wrap items-start justify-between gap-3 border-b px-3 py-3 sm:flex-nowrap sm:items-center sm:gap-4 sm:px-6 sm:py-4">
          <div class="flex min-w-0 items-center gap-3">
            ${isWorkerDetail &&
            html`<${Button} onClick=${() => navigateView("workers")} size="icon-sm" title="Back to Workers" type="button" variant="ghost"><${Icon} icon="lucide:arrow-left" size="16" /><span class="sr-only">Back</span><//>`}
            <div class="min-w-0">
              <h1 class="truncate text-lg font-semibold">${headerTitle}</h1>
              <p class="text-muted-foreground truncate text-sm">${headerDescription}</p>
            </div>
          </div>
          <div class="flex w-full flex-wrap items-center justify-end gap-2 sm:w-auto sm:flex-nowrap">
            ${isFiles &&
            filesStatus?.uploading &&
            html`<span class="text-muted-foreground flex items-center gap-1.5 text-xs"><${Spinner} size="14" /> Publishing…</span>`}
            ${view === "workers" &&
            html`<${Button} aria-label="Deploy app" onClick=${openDialog(DEPLOY_DIALOG_ID)} size="sm" type="button"><${Icon} icon="lucide:upload-cloud" size="14" /><span class="hidden sm:inline">Deploy app</span><//>`}
            ${isFiles &&
            html`
              <${Button} aria-label="Upload files" onClick=${() => filesInput.current?.click()} size="sm" type="button" variant="outline"><${Icon} icon="lucide:upload" size="14" /><span class="hidden md:inline">Upload files</span><//>
              <${Button} aria-label="Upload folder" onClick=${() => folderInput.current?.click()} size="sm" type="button" variant="outline"><${Icon} icon="lucide:folder-up" size="14" /><span class="hidden md:inline">Upload folder</span><//>
              <input class="hidden" multiple onChange=${handlePick} ref=${filesInput} type="file" />
              <input class="hidden" onChange=${handlePick} ref=${folderInput} type="file" webkitdirectory />
            `}
            <${Button} disabled=${refreshing && !refreshesCurrentView} onClick=${refreshCurrentView} size="sm" type="button" variant="outline">
              ${refreshing && !refreshesCurrentView ? html`<${Spinner} size="14" />` : html`<${Icon} icon="lucide:refresh-cw" size="14" />`}
              <span class="hidden sm:inline">Refresh</span>
            <//>
          </div>
        </header>
        <div class="min-h-0 flex-1 overflow-y-auto p-3 sm:p-6">
          ${(isObservabilityOverview || isGlobalObservability) &&
          html`
            <nav aria-label="Observability sections" class="mb-4">
              <${TabsList}>
                <${TabsTrigger} active=${isObservabilityOverview} aria-selected=${isObservabilityOverview} onClick=${() => navigateView("observability-overview")}>Overview<//>
                <${TabsTrigger} active=${isGlobalObservability} aria-selected=${isGlobalObservability} onClick=${() => navigateView("observability-global")}>Logs<//>
              <//>
            </nav>
          `}
          ${isWorkerDetail &&
          html`
            <nav aria-label="Worker version sections" class="mb-4">
              <${TabsList}>
                <${TabsTrigger}
                  active=${isFiles}
                  aria-selected=${isFiles}
                  onClick=${() => navigate({ path: filesPath, target: filesTarget, view: "files" })}
                >
                  Files
                <//>
                <${TabsTrigger}
                  active=${isObservability}
                  aria-selected=${isObservability}
                  onClick=${() => navigate({ target: filesTarget, view: "observability" })}
                >
                  Observability
                <//>
                <${TabsTrigger}
                  active=${isLogs}
                  aria-selected=${isLogs}
                  onClick=${() => navigate({ target: filesTarget, view: "logs" })}
                >
                  Logs
                <//>
              <//>
            </nav>
          `}
          ${view === "overview" && html`<${OverviewView} data=${data} onGoToWorkers=${() => navigateView("workers")} />`}
          ${view === "workers" &&
          html`<${WorkersView} apiKey=${apiKey} data=${data} onDeployed=${onRefresh} onViewAllLogs=${() => navigateView("observability-global")} onViewFiles=${openFiles} onViewLogs=${openLogs} onViewObservability=${openObservability} />`}
          ${isObservabilityOverview && html`<${ObservabilityOverview} apiKey=${apiKey} data=${data} onOpenWorker=${(worker, nextView) => navigate({ target: { name: worker.name, version: worker.version }, view: nextView })} reload=${viewReload} />`}
          ${isGlobalObservability && html`<${WorkerLogsView} apiKey=${apiKey} reload=${viewReload} target=${null} />`}
          ${isFiles &&
          html`<${FilesView} apiKey=${apiKey} onUpload=${uploadZip} path=${filesPath} reload=${filesReload} setPath=${navigateFilesPath} status=${filesStatus} target=${filesTarget} />`}
          ${isObservability && html`<${WorkerObservabilityView} apiKey=${apiKey} data=${data} reload=${viewReload} target=${filesTarget} />`}
          ${isLogs && html`<${WorkerLogsView} apiKey=${apiKey} reload=${viewReload} target=${filesTarget} />`}
        </div>
      <//>
    </div>
  `;
}

function App() {
  const [session, setSession] = useState(undefined);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    const apiKey = readSessionApiKey();
    if (!apiKey) {
      setSession(null);
      return;
    }

    let cancelled = false;
    loadAll(apiKey)
      .then((data) => {
        if (!cancelled) setSession({ apiKey, data });
      })
      .catch(() => {
        storeSessionApiKey("");
        if (!cancelled) setSession(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (session === undefined) {
    return html`<div class="bg-background flex min-h-full w-full items-center justify-center"><${Spinner} size="20" /><span class="sr-only">Restoring session</span></div>`;
  }

  if (!session) {
    return html`<${Login} onAuthenticated=${(apiKey, data) => { storeSessionApiKey(apiKey); setSession({ apiKey, data }); }} />`;
  }

  const refresh = async () => {
    setRefreshing(true);
    try {
      // Reconcile the workers folder (apps added/removed on disk) so Refresh
      // reflects true state, then reload. A broken manifest on disk must not
      // block seeing the current index, so ignore rescan failures here.
      await apiJson(session.apiKey, "/api/admin/workers/rescan", {
        body: JSON.stringify({ dryRun: false }),
        headers: { "content-type": "application/json" },
        method: "POST",
      }).catch(() => {});
      const data = await loadAll(session.apiKey);
      setSession({ apiKey: session.apiKey, data });
    } finally {
      setRefreshing(false);
    }
  };

  return html`
    <${Shell}
      apiKey=${session.apiKey}
      data=${session.data}
      onLogout=${() => { storeSessionApiKey(""); setSession(null); }}
      onRefresh=${refresh}
      refreshing=${refreshing}
    />
  `;
}

render(html`<${App} />`, document.getElementById("app"));
