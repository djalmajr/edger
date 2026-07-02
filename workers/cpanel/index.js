import { unzipSync, zipSync } from "fflate";
import { html } from "htm/preact";
import { render } from "preact";
import { useCallback, useRef, useState } from "preact/hooks";
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
import { Icon } from "~/components/ui/icon.js";
import { Input } from "~/components/ui/input.js";
import { Label } from "~/components/ui/label.js";
import { Select, SelectItem } from "~/components/ui/select.js";
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
  { description: "Static extension registry", icon: "lucide:puzzle", id: "modules", title: "Modules" },
  { description: "Local gateway diagnostics", icon: "lucide:waypoints", id: "gateway", title: "Gateway" },
  { description: "Operator credentials", icon: "lucide:key-round", id: "keys", title: "API Keys" },
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
  const [workers, modules, gateway, keys, workerErrors] = await Promise.all([
    apiJson(apiKey, "/api/admin/workers").then((data) => data.workers || []),
    apiJson(apiKey, "/api/admin/extensions").then((data) => data.extensions || []),
    apiJson(apiKey, "/api/admin/gateway/stats").catch((error) => ({ error: error.message })),
    apiJson(apiKey, "/api/admin/keys").then((data) => data.keys || []),
    apiJson(apiKey, "/api/admin/workers/error-summary")
      .then((data) => data.summary || {})
      .catch(() => ({})),
  ]);
  return { gateway, keys, modules, principal: session.principal, workerErrors, workers };
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

function splitCsv(value) {
  return String(value || "")
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
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

function OverviewView({ data }) {
  const { gateway, keys, modules, principal, workers } = data;
  const historyLabel = gateway?.history?.persistent?.enabled ? "persistent" : "local";
  return html`
    <div class="grid gap-4">
      <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
        <${MetricCard}
          help="Loaded from RUNTIME_WORKER_DIRS"
          icon="lucide:cpu"
          label="Workers"
          value=${String(workers.length)}
        />
        <${MetricCard}
          help="Static extension registry"
          icon="lucide:puzzle"
          label="Modules"
          value=${String(modules.length)}
        />
        <${MetricCard}
          help="Gateway total since boot"
          icon="lucide:activity"
          label="Requests"
          value=${String(gateway?.requests?.total ?? "-")}
        />
        <${MetricCard}
          help="Operator API keys"
          icon="lucide:fingerprint"
          label="Keys"
          value=${String(keys.length)}
        />
      </div>
      <${Section} description="Session principal resolved by the runtime auth gate." title="Runtime status">
        <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
          <${DetailItem} label="Principal" value=${principal?.name || "-"} />
          <${DetailItem} label="Role" value=${principal?.role || "-"} />
          <${DetailItem} label="Namespaces" value=${listText(principal?.namespaces)} />
          <${DetailItem} label="Permissions" value=${listText(principal?.permissions)} />
          <${DetailItem} label="Gateway history" value=${historyLabel} />
          <${DetailItem} label="Remote deploy" value="not included" />
        </div>
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

function WorkersView({ apiKey, data, onDeployed }) {
  const [busy, setBusy] = useState(null);
  const [errorsView, setErrorsView] = useState(null);
  const workerErrors = data.workerErrors || {};

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

  const deployAction = html`
    <${Button} onClick=${openDialog(DEPLOY_DIALOG_ID)} size="sm" type="button">
      <${Icon} icon="lucide:upload-cloud" size="14" /> Deploy app
    <//>
  `;
  return html`
    <${Section}
      action=${deployAction}
      description="Every worker discovered by the manifest loader. Refresh reconciles the workers folder."
      title="Workers"
    >
      <${DeployDialog} apiKey=${apiKey} onDeployed=${onDeployed} />
      <${Table}>
        <${TableHeader}>
          <${TableRow}>
            <${TableHead}>Name<//>
            <${TableHead}>Version<//>
            <${TableHead}>Kind<//>
            <${TableHead}>Visibility<//>
            <${TableHead}>Status<//>
            <${TableHead}>Source<//>
            <${TableHead} className="text-right">Actions<//>
          <//>
        <//>
        <${TableBody}>
          ${data.workers.map((worker) => {
            const multiVersion = versionCount.get(worker.name) > 1;
            const serving = multiVersion && servingVersion.get(worker.name) === worker.version;
            const disabled = worker.status === "disabled";
            const errorInfo = workerErrors[worker.name];
            const rowKey = `${worker.name}@${worker.version}`;
            return html`
              <${TableRow} key=${rowKey}>
                <${TableCell}><code class="bg-muted rounded px-1.5 py-0.5 text-xs">${worker.name}</code><//>
                <${TableCell}>
                  <span class="flex items-center gap-2">
                    ${worker.version || "-"}
                    ${serving && html`<${Badge} variant="default">serving<//>`}
                  </span>
                <//>
                <${TableCell}>${kindLabel(worker.kind)}<//>
                <${TableCell}>${visibilityBadge(worker.visibility)}<//>
                <${TableCell}>
                  <span class="flex items-center gap-2">
                    <${Badge} variant=${disabled ? "secondary" : "outline"}>${worker.status || "-"}<//>
                    ${errorInfo &&
                    errorInfo.count > 0 &&
                    html`
                      <button
                        class="inline-flex items-center gap-1"
                        onClick=${() => openErrors(worker.name)}
                        title=${errorInfo.latest ? `${errorInfo.latest.status} ${errorInfo.latest.code} — ${errorInfo.latest.message}` : "View errors"}
                        type="button"
                      >
                        <${Badge} variant="destructive">
                          <${Icon} icon="lucide:triangle-alert" size="11" />
                          ${errorInfo.count} error${errorInfo.count > 1 ? "s" : ""}
                        <//>
                      </button>
                    `}
                  </span>
                <//>
                <${TableCell} className="text-muted-foreground">${worker.source || "-"}<//>
                <${TableCell} className="text-right">
                  ${multiVersion
                    ? html`
                        <${Button}
                          disabled=${busy === rowKey}
                          onClick=${() => toggle(worker, disabled)}
                          size="sm"
                          title=${disabled ? "Enable this version" : "Disable this version (rollback traffic to another)"}
                          type="button"
                          variant=${disabled ? "outline" : "ghost"}
                        >
                          ${busy === rowKey
                            ? html`<${Spinner} size="14" />`
                            : html`<${Icon} icon=${disabled ? "lucide:rotate-ccw" : "lucide:power-off"} size="14" />`}
                          ${disabled ? "Enable" : "Disable"}
                        <//>
                      `
                    : html`<span class="text-muted-foreground text-xs">—</span>`}
                <//>
              <//>
            `;
          })}
        <//>
      <//>
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
    <//>
  `;
}

function ModulesView({ data }) {
  return html`
    <${Section} description="Extensions registered at the composition root." title="Modules">
      <${Table}>
        <${TableHeader}>
          <${TableRow}>
            <${TableHead}>Name<//>
            <${TableHead}>Kind<//>
            <${TableHead}>Status<//>
            <${TableHead}>Capabilities<//>
            <${TableHead}>Priority<//>
          <//>
        <//>
        <${TableBody}>
          ${data.modules.map(
            (mod) => html`
              <${TableRow} key=${mod.name}>
                <${TableCell}><code class="bg-muted rounded px-1.5 py-0.5 text-xs">${mod.name}</code><//>
                <${TableCell}>${kindLabel(mod.kind)}<//>
                <${TableCell}><${Badge} variant="outline">${mod.status || "-"}<//><//>
                <${TableCell} className="text-muted-foreground">${listText(mod.capabilities)}<//>
                <${TableCell}>${mod.priority ?? "-"}<//>
              <//>
            `,
          )}
        <//>
      <//>
    <//>
  `;
}

function GatewayView({ data }) {
  const gateway = data.gateway || {};
  if (gateway.error) {
    return html`
      <${Alert} variant="destructive">
        <${AlertTitle}>Gateway stats unavailable<//>
        <${AlertDescription}>${gateway.error}<//>
      <//>
    `;
  }
  const requests = gateway.requests || {};
  const rateLimit = gateway.rateLimit || {};
  const historyLabel = gateway.history?.persistent?.enabled ? "persistent" : "local";
  return html`
    <${Section} description="Counters observed by the gateway middleware since boot." title="Gateway">
      <div class="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        <${DetailItem} label="Total" value=${requests.total ?? "-"} />
        <${DetailItem} label="Continued" value=${requests.continued ?? "-"} />
        <${DetailItem} label="Redirected" value=${requests.redirected ?? "-"} />
        <${DetailItem} label="Rate limited" value=${requests.rateLimited ?? "-"} />
        <${DetailItem} label="Buckets" value=${rateLimit.activeBuckets ?? "-"} />
        <${DetailItem} label="History" value=${historyLabel} />
      </div>
    <//>
  `;
}

function KeysView({ apiKey, data, onChanged }) {
  const [createdKey, setCreatedKey] = useState(null);
  const [error, setError] = useState(null);
  const [pending, setPending] = useState(false);

  const createKey = async (event) => {
    event.preventDefault();
    const form = event.currentTarget;
    const fields = new FormData(form);
    setError(null);
    setPending(true);
    try {
      const result = await apiJson(apiKey, "/api/admin/keys", {
        body: JSON.stringify({
          expiresAt: null,
          name: String(fields.get("name") || "").trim(),
          namespaces: splitCsv(String(fields.get("namespaces") || "*") || "*"),
          permissions: splitCsv(String(fields.get("permissions") || "")),
          role: String(fields.get("role") || "viewer"),
        }),
        headers: { "content-type": "application/json" },
        method: "POST",
      });
      setCreatedKey(result);
      form.reset();
      await onChanged();
    } catch (err) {
      setError(err.message);
    } finally {
      setPending(false);
    }
  };

  const revoke = async (id) => {
    setError(null);
    try {
      await apiJson(apiKey, `/api/admin/keys/${id}/revoke`, { method: "POST" });
      await onChanged();
    } catch (err) {
      setError(err.message);
    }
  };

  return html`
    <div class="grid gap-4">
      <${Section} description="Create an operator key scoped by role, permissions and namespaces." title="New key">
        <form class="grid gap-4 md:grid-cols-[minmax(0,1fr)_160px_minmax(0,1fr)_minmax(0,1fr)_auto] md:items-end" onSubmit=${createKey}>
          <div class="grid gap-2">
            <${Label} for="key-name">Name<//>
            <${Input} id="key-name" name="name" placeholder="ci-deploy" required />
          </div>
          <div class="grid gap-2">
            <${Label} for="key-role">Role<//>
            <${Select} id="key-role" name="role">
              <${SelectItem} value="viewer">viewer<//>
              <${SelectItem} value="editor">editor<//>
              <${SelectItem} value="admin">admin<//>
            <//>
          </div>
          <div class="grid gap-2">
            <${Label} for="key-permissions">Permissions<//>
            <${Input} id="key-permissions" name="permissions" placeholder="comma-separated" />
          </div>
          <div class="grid gap-2">
            <${Label} for="key-namespaces">Namespaces<//>
            <${Input} id="key-namespaces" name="namespaces" placeholder="default *" />
          </div>
          <${Button} disabled=${pending} type="submit">
            ${pending ? html`<${Spinner} size="16" />` : html`<${Icon} icon="lucide:plus" size="16" />`}
            Create
          <//>
        </form>
        ${createdKey &&
        html`
          <${Alert} className="mt-4">
            <${AlertTitle}>Key created — copy it now, it is shown only once<//>
            <${AlertDescription}>
              <code class="bg-muted rounded px-1.5 py-0.5 text-xs">${createdKey.rawKey}</code>
            <//>
          <//>
        `}
        ${error &&
        html`
          <${Alert} className="mt-4" variant="destructive">
            <${AlertTitle}>Key operation failed<//>
            <${AlertDescription}>${error}<//>
          <//>
        `}
      <//>
      <${Section} description="Keys accepted by the runtime auth gate." title="API keys">
        <${Table}>
          <${TableHeader}>
            <${TableRow}>
              <${TableHead}>Name<//>
              <${TableHead}>Role<//>
              <${TableHead}>Prefix<//>
              <${TableHead}>Namespaces<//>
              <${TableHead}>Status<//>
              <${TableHead} className="w-12"><span class="sr-only">Actions</span><//>
            <//>
          <//>
          <${TableBody}>
            ${data.keys.map(
              (key) => html`
                <${TableRow} key=${key.id ?? key.name}>
                  <${TableCell}><code class="bg-muted rounded px-1.5 py-0.5 text-xs">${key.name}</code><//>
                  <${TableCell}>${key.role || "-"}<//>
                  <${TableCell} className="text-muted-foreground">${key.keyPrefix || "-"}<//>
                  <${TableCell} className="text-muted-foreground">${listText(key.namespaces)}<//>
                  <${TableCell}>
                    <${Badge} variant=${key.isRoot ? "default" : key.revoked ? "destructive" : "outline"}>
                      ${key.isRoot ? "root" : key.revoked ? "revoked" : "active"}
                    <//>
                  <//>
                  <${TableCell}>
                    ${!key.isRoot &&
                    !key.revoked &&
                    html`
                      <${Button}
                        onClick=${() => revoke(key.id)}
                        size="icon-sm"
                        title="Revoke"
                        type="button"
                        variant="ghost"
                      >
                        <${Icon} className="text-destructive" icon="lucide:trash-2" size="16" />
                        <span class="sr-only">Revoke</span>
                      <//>
                    `}
                  <//>
                <//>
              `,
            )}
          <//>
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

function Shell({ apiKey, data, onLogout, onRefresh, refreshing, setData }) {
  const [view, setView] = useState("overview");
  const active = VIEWS.find((entry) => entry.id === view) || VIEWS[0];
  const principal = data.principal;

  const reloadKeys = useCallback(async () => {
    const keys = await apiJson(apiKey, "/api/admin/keys").then((res) => res.keys || []);
    setData((current) => ({ ...current, keys }));
  }, [apiKey, setData]);

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
                  <${SidebarMenuButton} isActive=${view === entry.id} onClick=${() => setView(entry.id)}>
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
          <div class="min-w-0">
            <h1 class="truncate text-lg font-semibold">${active.title}</h1>
            <p class="text-muted-foreground truncate text-sm">${active.description}</p>
          </div>
          <div class="flex items-center gap-2">
            <${Button} disabled=${refreshing} onClick=${onRefresh} size="sm" type="button" variant="outline">
              ${refreshing ? html`<${Spinner} size="14" />` : html`<${Icon} icon="lucide:refresh-cw" size="14" />`}
              Refresh
            <//>
          </div>
        </header>
        <div class="min-h-0 flex-1 overflow-y-auto p-6">
          ${view === "overview" && html`<${OverviewView} data=${data} />`}
          ${view === "workers" &&
          html`<${WorkersView} apiKey=${apiKey} data=${data} onDeployed=${onRefresh} />`}
          ${view === "modules" && html`<${ModulesView} data=${data} />`}
          ${view === "gateway" && html`<${GatewayView} data=${data} />`}
          ${view === "keys" && html`<${KeysView} apiKey=${apiKey} data=${data} onChanged=${reloadKeys} />`}
        </div>
      <//>
    </div>
  `;
}

function App() {
  const [session, setSession] = useState(null);
  const [refreshing, setRefreshing] = useState(false);

  const setData = useCallback((updater) => {
    setSession((current) =>
      current ? { ...current, data: typeof updater === "function" ? updater(current.data) : updater } : current,
    );
  }, []);

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
      setData=${setData}
    />
  `;
}

render(html`<${App} />`, document.getElementById("app"));
