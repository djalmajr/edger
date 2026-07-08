import { unzipSync, zipSync } from "fflate";
import { html } from "htm/preact";
import { render } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { Alert, AlertDescription, AlertTitle } from "~/components/ui/alert.js";
import { Badge } from "~/components/ui/badge.js";
import { Button } from "~/components/ui/button.js";
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
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "~/components/ui/dropdown-menu.js";
import { Icon } from "~/components/ui/icon.js";
import { Input } from "~/components/ui/input.js";
import { Label } from "~/components/ui/label.js";
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
];

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

function listText(value) {
  return Array.isArray(value) && value.length ? value.join(", ") : "-";
}

function Login({ onAuthenticated }) {
  const [error, setError] = useState(null);
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
        <${Input} autocomplete="off" className="mt-2" id="cpanel-api-key" name="apiKey" type="password" />
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
      ${sub && html`<div class="text-muted-foreground mt-2 text-xs">${sub}</div>`}
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

function StatusRow({ label, value }) {
  return html`
    <div class="border-border/70 flex items-center justify-between border-b py-2 last:border-0">
      <span class="text-muted-foreground text-sm">${label}</span>
      <span class="text-sm font-medium">${value}</span>
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

  return html`
    <div class="grid gap-4">
      <div class="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <${StatTile} icon="lucide:layout-grid" label="Workers" sub=${html`Loaded from <code class="bg-muted rounded px-1 py-0.5 text-[11px]">RUNTIME_WORKER_DIRS</code>`} value=${String(workers.length)} />
        <${StatTile} icon="lucide:box" label="Apps" sub="distinct apps, each version on its own URL" value=${String(apps.size)} />
        <${StatTile} dot="bg-emerald-500" label="Serving" sub="enabled versions" value=${String(enabled.length)} />
        <${StatTile} icon="lucide:triangle-alert" label="Needs attention" sub=${`${disabled.length} disabled · ${errored.length} with errors`} tone=${attention > 0 ? "warn" : null} value=${String(attention)} />
      </div>

      <div class="grid gap-4 lg:grid-cols-3">
        <${Card} className="lg:col-span-2">
          <${CardHeader}>
            <${CardTitle}>Pool health<//>
            <${CardAction}><code class="text-muted-foreground text-[11px]">live · /metrics/stats</code><//>
          <//>
          <${CardContent}>
            <div class="grid grid-cols-3 gap-y-5">
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
        <${CardHeader}>
          <${CardTitle}>
            <span class="flex items-center gap-2">Needs attention ${attention > 0 && html`<${Badge} variant="secondary">${attention}<//>`}</span>
          <//>
        <//>
        <${CardContent} className="py-0">
          ${attention === 0 &&
          html`<p class="text-muted-foreground py-4 text-sm">Everything looks healthy — no disabled versions or recent errors.</p>`}
          ${disabled.map(
            (worker) => html`
              <div class="border-border/70 flex items-center gap-3 border-b py-3 last:border-0" key=${`d-${worker.name}@${worker.version}`}>
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
              <div class="border-border/70 flex items-center gap-3 border-b py-3 last:border-0" key=${`e-${name}`}>
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

function WorkersView({ apiKey, data, onDeployed, onViewFiles }) {
  const [busy, setBusy] = useState(null);
  const [errorsView, setErrorsView] = useState(null);
  const workerErrors = data.workerErrors || {};
  const runtimeWorkers = data.metricsStats?.workers || [];

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

  const versionCount = new Map();
  for (const worker of data.workers) {
    versionCount.set(worker.name, (versionCount.get(worker.name) || 0) + 1);
  }
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

  return html`
    <div>
      <${DeployDialog} apiKey=${apiKey} onDeployed=${onDeployed} />
      <div class="grid gap-3">
        ${groups.map((group) => {
          const serving = servingVersion.get(group.name);
          const kind = kindLabel(group.versions[0].kind);
          const scope = group.versions[0].namespace;
          const errorInfo = workerErrors[group.name];
          return html`
            <div class="border-border bg-card rounded-xl border" key=${group.name}>
              <div class="flex items-center gap-3 px-4 py-3">
                <span class="bg-primary/10 text-primary flex size-9 shrink-0 items-center justify-center rounded-lg"><${Icon} icon="lucide:box" size="18" /></span>
                <div class="min-w-0">
                  <div class="flex items-center gap-2">
                    <span class="font-semibold">${group.name}</span>
                    <${Badge} variant="secondary">${kind}<//>
                  </div>
                  <p class="text-muted-foreground truncate text-xs">
                    ${scope ? html`<span class="font-mono">@${scope}</span> · ` : ""}${group.versions.length} version${group.versions.length > 1 ? "s" : ""}
                  </p>
                </div>
                <div class="ml-auto flex items-center gap-2">
                  ${serving &&
                  html`<span class="flex items-center gap-1.5 rounded-full bg-emerald-50 px-2.5 py-1 text-xs font-medium text-emerald-700"><span class="size-1.5 rounded-full bg-emerald-500"></span>latest → <span class="font-mono">${serving}</span></span>`}
                  <${Button} onClick=${openDialog(DEPLOY_DIALOG_ID)} size="sm" type="button" variant="outline"><${Icon} icon="lucide:upload" size="14" /> New version<//>
                </div>
              </div>
              <div class="text-muted-foreground bg-muted/30 border-border/70 grid grid-cols-[150px_1fr_130px_44px] gap-4 border-t px-4 py-2 text-[11px] font-semibold tracking-wide uppercase">
                <span>Version</span><span>URL</span><span>Status</span><span></span>
              </div>
              ${group.versions.map((worker) => {
                const isLatest = serving === worker.version;
                const disabled = worker.status === "disabled";
                const url = workerUrl(worker, isLatest);
                const rowKey = `${worker.name}@${worker.version}`;
                return html`
                  <div class="border-border/60 grid grid-cols-[150px_1fr_130px_44px] items-center gap-4 border-t px-4 py-2.5" key=${rowKey}>
                    <span class=${`flex items-center gap-2 font-mono text-sm ${disabled ? "text-muted-foreground" : ""}`}>
                      ${worker.version}
                      ${isLatest && html`<${Badge} variant="default" className="px-1.5 text-[10px]">latest<//>`}
                    </span>
                    <a class=${`truncate font-mono text-sm hover:underline ${isLatest ? "text-primary" : "text-foreground/80"} ${disabled ? "line-through opacity-60" : ""}`} href=${url} rel="noreferrer" target="_blank">${url}</a>
                    <span class="text-sm">
                      ${disabled
                        ? html`<span class="flex items-center gap-1.5 text-amber-700"><span class="size-2 rounded-full bg-amber-500"></span>Disabled</span>`
                        : isLatest
                          ? html`<span class="flex items-center gap-1.5 font-medium text-emerald-700"><span class="size-2 rounded-full bg-emerald-500 ring-2 ring-emerald-100"></span>Serving</span>`
                          : html`<span class="text-muted-foreground flex items-center gap-1.5"><span class="border-muted-foreground/40 size-2 rounded-full border-2"></span>Enabled</span>`}
                    </span>
                    <${DropdownMenu} className="justify-self-end">
                      <${DropdownMenuTrigger} className="text-muted-foreground" size="icon" variant="ghost"><${Icon} icon="lucide:ellipsis-vertical" size="16" /><//>
                      <${DropdownMenuContent} align="end">
                        <${DropdownMenuItem} onClick=${() => onViewFiles(worker, isLatest)}><${Icon} icon="lucide:folder" size="15" /> View files<//>
                        <${DropdownMenuItem} onClick=${() => window.open(url, "_blank", "noopener")}><${Icon} icon="lucide:external-link" size="15" /> Open URL<//>
                        <${DropdownMenuItem} onClick=${() => navigator.clipboard?.writeText(location.origin + url)}><${Icon} icon="lucide:copy" size="15" /> Copy URL<//>
                        <${DropdownMenuSeparator} />
                        <${DropdownMenuItem} disabled=${busy === rowKey} onClick=${() => toggle(worker, disabled)}><${Icon} icon=${disabled ? "lucide:rotate-ccw" : "lucide:power-off"} size="15" /> ${disabled ? "Enable version" : "Disable version"}<//>
                        ${errorInfo &&
                        errorInfo.count > 0 &&
                        html`<${DropdownMenuItem} className="text-destructive" onClick=${() => openErrors(worker.name)}><${Icon} icon="lucide:triangle-alert" size="15" /> View errors (${errorInfo.count})<//>`}
                      <//>
                    <//>
                  </div>
                `;
              })}
            </div>
          `;
        })}
      </div>
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
  const [view, setView] = useState("overview");
  const [filesTarget, setFilesTarget] = useState(null);
  const [filesPath, setFilesPath] = useState("");
  const [filesReload, setFilesReload] = useState(0);
  const [filesStatus, setFilesStatus] = useState(null);
  const filesInput = useRef(null);
  const folderInput = useRef(null);
  const active = VIEWS.find((entry) => entry.id === view) || VIEWS[0];
  const principal = data.principal;

  const openFiles = (worker, isLatest) => {
    setFilesTarget({ name: worker.name, url: workerUrl(worker, isLatest), version: worker.version });
    setFilesPath("");
    setFilesStatus(null);
    setView("files");
  };
  const isFiles = view === "files" && filesTarget;
  const headerTitle = isFiles ? filesTarget.name : active.title;
  const headerDescription = isFiles ? `Version ${filesTarget.version} · files` : active.description;

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
      <${Sidebar}>
        <${SidebarHeader}>
          <div class="flex items-center gap-2 px-2 py-1.5">
            <span class="bg-primary text-primary-foreground flex size-8 items-center justify-center rounded-md text-xs font-semibold">ed</span>
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
                  <${SidebarMenuButton} isActive=${view === entry.id || (view === "files" && entry.id === "workers")} onClick=${() => setView(entry.id)}>
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
        <header class="border-border flex items-center justify-between gap-4 border-b px-6 py-4">
          <div class="flex min-w-0 items-center gap-3">
            ${isFiles &&
            html`<${Button} onClick=${() => setView("workers")} size="icon-sm" title="Back to Workers" type="button" variant="ghost"><${Icon} icon="lucide:arrow-left" size="16" /><span class="sr-only">Back</span><//>`}
            <div class="min-w-0">
              <h1 class="truncate text-lg font-semibold">${headerTitle}</h1>
              <p class="text-muted-foreground truncate text-sm">${headerDescription}</p>
            </div>
          </div>
          <div class="flex items-center gap-2">
            ${isFiles &&
            filesStatus?.uploading &&
            html`<span class="text-muted-foreground flex items-center gap-1.5 text-xs"><${Spinner} size="14" /> Publishing…</span>`}
            <${Button} disabled=${refreshing && !isFiles} onClick=${isFiles ? () => setFilesReload((value) => value + 1) : onRefresh} size="sm" type="button" variant="outline">
              ${refreshing && !isFiles ? html`<${Spinner} size="14" />` : html`<${Icon} icon="lucide:refresh-cw" size="14" />`}
              Refresh
            <//>
            ${view === "workers" &&
            html`<${Button} onClick=${openDialog(DEPLOY_DIALOG_ID)} size="sm" type="button"><${Icon} icon="lucide:upload-cloud" size="14" /> Deploy app<//>`}
            ${isFiles &&
            html`
              <${Button} onClick=${() => filesInput.current?.click()} size="sm" type="button" variant="outline"><${Icon} icon="lucide:upload" size="14" /> Upload files<//>
              <${Button} onClick=${() => folderInput.current?.click()} size="sm" type="button" variant="outline"><${Icon} icon="lucide:folder-up" size="14" /> Upload folder<//>
              <input class="hidden" multiple onChange=${handlePick} ref=${filesInput} type="file" />
              <input class="hidden" onChange=${handlePick} ref=${folderInput} type="file" webkitdirectory />
            `}
          </div>
        </header>
        <div class="min-h-0 flex-1 overflow-y-auto p-6">
          ${view === "overview" && html`<${OverviewView} data=${data} onGoToWorkers=${() => setView("workers")} />`}
          ${view === "workers" &&
          html`<${WorkersView} apiKey=${apiKey} data=${data} onDeployed=${onRefresh} onViewFiles=${openFiles} />`}
          ${isFiles &&
          html`<${FilesView} apiKey=${apiKey} onUpload=${uploadZip} path=${filesPath} reload=${filesReload} setPath=${setFilesPath} status=${filesStatus} target=${filesTarget} />`}
        </div>
      <//>
    </div>
  `;
}

function App() {
  const [session, setSession] = useState(null);
  const [refreshing, setRefreshing] = useState(false);

  if (!session) {
    return html`<${Login} onAuthenticated=${(apiKey, data) => setSession({ apiKey, data })} />`;
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
      onLogout=${() => setSession(null)}
      onRefresh=${refresh}
      refreshing=${refreshing}
    />
  `;
}

render(html`<${App} />`, document.getElementById("app"));
