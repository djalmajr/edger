import { unzipSync, zipSync } from "fflate";
import {
  QueryClient,
  QueryClientProvider,
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import {
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import * as React from "react";
import { createRoot } from "react-dom/client";
import { CartesianGrid, Line, LineChart, XAxis } from "recharts";

import { Badge } from "@edger/ui/components/ui/badge";
import { Button } from "@edger/ui/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@edger/ui/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@edger/ui/components/ui/chart";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@edger/ui/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@edger/ui/components/ui/dropdown-menu";
import { Input } from "@edger/ui/components/ui/input";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupButton,
  InputGroupInput,
} from "@edger/ui/components/ui/input-group";
import { Label } from "@edger/ui/components/ui/label";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@edger/ui/components/ui/select";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
} from "@edger/ui/components/ui/sidebar";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@edger/ui/components/ui/table";
import { Tabs, TabsList, TabsTrigger } from "@edger/ui/components/ui/tabs";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@edger/ui/components/ui/tooltip";
import {
  ActivityIcon,
  ArrowLeftIcon,
  BoxIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  CpuIcon,
  ExternalLinkIcon,
  EyeIcon,
  EyeOffIcon,
  FileArchiveIcon,
  FileIcon,
  FolderIcon,
  FolderOpenIcon,
  GaugeIcon,
  HeartPulseIcon,
  KeyRoundIcon,
  LogInIcon,
  LogOutIcon,
  MoreVerticalIcon,
  PowerOffIcon,
  RefreshCwIcon,
  RocketIcon,
  RotateCcwIcon,
  RouteIcon,
  ScrollTextIcon,
  SearchIcon,
  ShieldCheckIcon,
  UploadCloudIcon,
  UploadIcon,
} from "@edger/ui/icons/lucide";
import { ThemeProvider } from "@edger/ui/lib/theme";

import "./app.css";
import {
  apiJson,
  compareSemver,
  kindLabel,
  loadAll,
  type RuntimeData,
  type RuntimeWorker,
  type Worker,
  workerUrl,
} from "./lib/api";

const SESSION_KEY = "edger.cpanel.apiKey";

type View = "overview" | "workers" | "observability" | "logs" | "files";
type Target = { name: string; version: string };
type RouteState = { path: string; target?: Target; view: View };

const NAVIGATION = [
  {
    description: "Runtime posture at a glance",
    icon: GaugeIcon,
    id: "overview" as const,
    title: "Overview",
  },
  {
    description: "Runtime worker inventory",
    icon: CpuIcon,
    id: "workers" as const,
    title: "Workers",
  },
  {
    description: "Local runtime signals and logs",
    icon: ActivityIcon,
    id: "observability" as const,
    title: "Observability",
  },
];

function readRoute(): RouteState {
  const parts = location.pathname.split("/").filter(Boolean);
  if (parts[0] !== "cpanel") return { path: "", view: "overview" };
  if (parts[1] === "observability")
    return { path: "", view: parts[2] === "logs" ? "logs" : "observability" };
  if (parts[1] !== "workers") return { path: "", view: "overview" };
  if (
    parts.length < 5 ||
    !["files", "logs", "observability"].includes(parts[4])
  )
    return { path: "", view: "workers" };
  return {
    path:
      parts[4] === "files"
        ? parts.slice(5).map(decodeURIComponent).join("/")
        : "",
    target: {
      name: decodeURIComponent(parts[2]),
      version: decodeURIComponent(parts[3]),
    },
    view: parts[4] as View,
  };
}

function routePath(route: RouteState) {
  if (route.view === "overview") return "/cpanel/";
  if (route.view === "workers" && !route.target) return "/cpanel/workers";
  if (route.view === "observability" && !route.target)
    return "/cpanel/observability";
  if (route.view === "logs" && !route.target)
    return "/cpanel/observability/logs";
  if (!route.target) return "/cpanel/workers";
  const suffix =
    route.view === "files" && route.path
      ? `/${route.path.split("/").map(encodeURIComponent).join("/")}`
      : "";
  return `/cpanel/workers/${encodeURIComponent(route.target.name)}/${encodeURIComponent(route.target.version)}/${route.view}${suffix}`;
}

function formatBytes(bytes: number) {
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

function bytesBody(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(
    bytes.byteOffset,
    bytes.byteOffset + bytes.byteLength,
  ) as ArrayBuffer;
}

function ActionButton({
  label,
  children,
  ...props
}: React.ComponentProps<typeof Button> & { label: string }) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <Button
            aria-label={label}
            size="icon-sm"
            variant="ghost"
            {...props}
          />
        }
      >
        {children}
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}

function MetricCard({
  description,
  icon: Icon,
  label,
  value,
}: {
  description: string;
  icon: React.ComponentType<{ className?: string }>;
  label: string;
  value: string;
}) {
  return (
    <Card>
      <CardContent className="p-4">
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Icon className="size-4" />
          {label}
        </div>
        <strong className="mt-3 block truncate font-heading text-2xl font-semibold">
          {value}
        </strong>
        <p className="mt-1 text-xs text-muted-foreground">{description}</p>
      </CardContent>
    </Card>
  );
}

function Login({ onAuthenticated }: { onAuthenticated(apiKey: string): void }) {
  const [error, setError] = React.useState("");
  const [visible, setVisible] = React.useState(false);
  const [pending, setPending] = React.useState(false);
  async function submit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const apiKey = String(
      new FormData(event.currentTarget).get("apiKey") ?? "",
    ).trim();
    if (!apiKey) return;
    setPending(true);
    setError("");
    try {
      await loadAll(apiKey);
      sessionStorage.setItem(SESSION_KEY, apiKey);
      onAuthenticated(apiKey);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setPending(false);
    }
  }
  return (
    <main className="grid min-h-screen place-items-center bg-background p-4">
      <Card className="w-full max-w-md">
        <CardHeader>
          <div className="flex items-center gap-3">
            <span className="grid size-10 place-items-center rounded-lg bg-primary/15 text-primary">
              <KeyRoundIcon className="size-5" />
            </span>
            <div>
              <CardTitle>EdgeR cPanel</CardTitle>
              <CardDescription>
                Enter the operator key to manage this runtime.
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <form className="grid gap-4" onSubmit={submit}>
            <div className="grid gap-2">
              <Label htmlFor="api-key">Root key</Label>
              <InputGroup>
                <InputGroupInput
                  autoComplete="current-password"
                  id="api-key"
                  name="apiKey"
                  type={visible ? "text" : "password"}
                />
                <InputGroupButton
                  aria-label={visible ? "Hide root key" : "Show root key"}
                  onClick={() => setVisible((value) => !value)}
                  size="icon-sm"
                >
                  {visible ? <EyeOffIcon /> : <EyeIcon />}
                </InputGroupButton>
              </InputGroup>
            </div>
            {error && (
              <p className="rounded-lg border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                {error}
              </p>
            )}
            <Button disabled={pending} type="submit">
              <LogInIcon />
              {pending ? "Connecting…" : "Connect"}
            </Button>
          </form>
        </CardContent>
      </Card>
    </main>
  );
}

function Overview({
  data,
  onWorkers,
}: {
  data: RuntimeData;
  onWorkers(): void;
}) {
  const workers = data.workers;
  const pool = data.metricsStats?.pool ?? {};
  const apps = new Set(workers.map((worker) => worker.name));
  const enabled = workers.filter((worker) => worker.status !== "disabled");
  const requests = (data.metricsStats?.workers ?? []).reduce(
    (sum, worker) => sum + (worker.requestTotal ?? 0),
    0,
  );
  const attention =
    workers.filter((worker) => worker.status === "disabled").length +
    Object.values(data.workerErrors).filter((value) => (value.count ?? 0) > 0)
      .length;
  return (
    <div className="grid gap-4">
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <MetricCard
          description="Worker versions loaded by the runtime"
          icon={CpuIcon}
          label="Workers"
          value={String(workers.length)}
        />
        <MetricCard
          description="Distinct applications"
          icon={BoxIcon}
          label="Apps"
          value={String(apps.size)}
        />
        <MetricCard
          description="Enabled worker versions"
          icon={RouteIcon}
          label="Routable"
          value={String(enabled.length)}
        />
        <MetricCard
          description="Observed across worker versions"
          icon={ActivityIcon}
          label="Total requests"
          value={String(requests)}
        />
      </div>
      <div className="grid gap-4 lg:grid-cols-3">
        <Card className="lg:col-span-2">
          <CardHeader>
            <CardTitle>Pool health</CardTitle>
            <CardAction>
              <Badge variant="outline">live · /metrics/stats</Badge>
            </CardAction>
          </CardHeader>
          <CardContent className="grid grid-cols-2 gap-5 sm:grid-cols-3">
            {[
              ["Active requests", pool.activeRequests],
              ["Idle workers", pool.idleWorkers],
              ["Cache hits", pool.cacheHits],
              ["Cache misses", pool.cacheMisses],
              ["Spawn p50", `${pool.spawnLatencyMsP50 ?? 0} ms`],
              ["Terminated", pool.terminatedTotal],
            ].map(([label, value]) => (
              <div key={String(label)}>
                <span className="text-xs font-medium uppercase text-muted-foreground">
                  {label}
                </span>
                <strong className="mt-1 block text-xl">
                  {String(value ?? 0)}
                </strong>
              </div>
            ))}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Runtime status</CardTitle>
          </CardHeader>
          <CardContent className="grid gap-3 text-sm">
            {[
              ["Principal", data.principal.name],
              ["Role", data.principal.role],
              ["Namespaces", data.principal.namespaces?.join(", ")],
              ["Control plane", "root-key gate"],
            ].map(([label, value]) => (
              <div
                className="flex justify-between gap-3 border-b pb-2 last:border-0"
                key={label}
              >
                <span className="text-muted-foreground">{label}</span>
                <strong className="truncate">{value ?? "-"}</strong>
              </div>
            ))}
          </CardContent>
        </Card>
      </div>
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            Needs attention{" "}
            {attention > 0 && <Badge variant="secondary">{attention}</Badge>}
          </CardTitle>
        </CardHeader>
        <CardContent>
          {attention === 0 ? (
            <p className="text-sm text-muted-foreground">
              Everything looks healthy — no disabled versions or recent errors.
            </p>
          ) : (
            <div className="flex items-center justify-between">
              <p className="text-sm text-muted-foreground">
                {attention} runtime signal{attention === 1 ? "" : "s"} need
                review.
              </p>
              <Button onClick={onWorkers} variant="outline">
                Review workers
              </Button>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function Workers({
  apiKey,
  data,
  onOpen,
  onRefresh,
}: {
  apiKey: string;
  data: RuntimeData;
  onOpen(worker: Worker, view: "files" | "logs" | "observability"): void;
  onRefresh(): Promise<void>;
}) {
  const [query, setQuery] = React.useState("");
  const [kind, setKind] = React.useState("all");
  const [expanded, setExpanded] = React.useState<Set<string>>(() => new Set());
  const [deployOpen, setDeployOpen] = React.useState(false);
  const [page, setPage] = React.useState(0);
  const queryClient = useQueryClient();
  const serving = new Map<string, string>();
  data.workers
    .filter((worker) => worker.status !== "disabled")
    .forEach((worker) => {
      const current = serving.get(worker.name);
      if (!current || compareSemver(worker.version, current) > 0)
        serving.set(worker.name, worker.version);
    });
  const grouped = new Map<string, Worker[]>();
  data.workers.forEach((worker) =>
    grouped.set(worker.name, [...(grouped.get(worker.name) ?? []), worker]),
  );
  const groups = [...grouped.values()].map((versions) => ({
    name: versions[0].name,
    versions: versions.sort((a, b) => compareSemver(b.version, a.version)),
  }));
  const normalized = query.trim().toLowerCase();
  const visible = groups.filter(
    (group) =>
      (!normalized ||
        group.name.toLowerCase().includes(normalized) ||
        group.versions.some((worker) => worker.version.includes(normalized))) &&
      (kind === "all" || kindLabel(group.versions[0].kind) === kind),
  );
  const pageSize = 10;
  const pages = Math.max(1, Math.ceil(visible.length / pageSize));
  const rows = visible.slice(page * pageSize, page * pageSize + pageSize);
  const kinds = [
    ...new Set(groups.map((group) => kindLabel(group.versions[0].kind))),
  ].sort();
  const toggleMutation = useMutation({
    mutationFn: ({ enable, worker }: { enable: boolean; worker: Worker }) =>
      apiJson(
        apiKey,
        `/api/admin/workers/${encodeURIComponent(worker.name)}/${enable ? "enable" : "disable"}?version=${encodeURIComponent(worker.version)}`,
        { method: "POST" },
      ),
    onSuccess: async () => {
      await onRefresh();
      await queryClient.invalidateQueries({ queryKey: ["cpanel"] });
    },
  });
  return (
    <div className="grid gap-4">
      <div className="flex flex-col gap-3 rounded-xl border bg-card p-4 lg:flex-row lg:items-center">
        <div>
          <h2 className="font-heading text-lg font-medium">Applications</h2>
          <p className="text-sm text-muted-foreground">
            Every version has its own pathname; the latest enabled version is
            the default.
          </p>
        </div>
        <div className="ml-auto flex flex-wrap gap-2">
          <InputGroup className="w-64">
            <InputGroupAddon>
              <SearchIcon />
            </InputGroupAddon>
            <InputGroupInput
              aria-label="Search workers"
              placeholder="Search applications…"
              value={query}
              onChange={(event) => {
                setQuery(event.currentTarget.value);
                setPage(0);
              }}
            />
          </InputGroup>
          <Select
            value={kind}
            onValueChange={(value) => setKind(value ?? "all")}
          >
            <SelectTrigger className="w-44">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                <SelectItem value="all">All kinds</SelectItem>
                {kinds.map((value) => (
                  <SelectItem key={value} value={value}>
                    {value}
                  </SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
          <Button onClick={() => setDeployOpen(true)}>
            <UploadCloudIcon />
            Deploy app
          </Button>
        </div>
      </div>
      <div className="grid gap-2">
        {rows.map((group) => {
          const open = expanded.has(group.name);
          const defaultVersion = serving.get(group.name);
          const disabled = group.versions.filter(
            (worker) => worker.status === "disabled",
          ).length;
          const errors = data.workerErrors[group.name]?.count ?? 0;
          return (
            <Card key={group.name}>
              <button
                className="flex w-full items-center gap-3 p-4 text-left"
                onClick={() =>
                  setExpanded((current) => {
                    const next = new Set(current);
                    next.has(group.name)
                      ? next.delete(group.name)
                      : next.add(group.name);
                    return next;
                  })
                }
                type="button"
              >
                {open ? <ChevronDownIcon /> : <ChevronRightIcon />}
                <span className="grid size-9 place-items-center rounded-lg bg-primary/15 text-primary">
                  <CpuIcon className="size-5" />
                </span>
                <span className="min-w-0 flex-1">
                  <strong className="block truncate">{group.name}</strong>
                  <small className="text-muted-foreground">
                    {kindLabel(group.versions[0].kind)} ·{" "}
                    {group.versions.length} version
                    {group.versions.length === 1 ? "" : "s"}
                  </small>
                </span>
                {defaultVersion && (
                  <Badge variant="secondary">
                    <span className="font-mono">{defaultVersion}</span>
                  </Badge>
                )}
                {(disabled > 0 || errors > 0) && (
                  <Badge variant="outline">
                    <CircleAlertIcon />
                    {disabled + errors}
                  </Badge>
                )}
              </button>
              {open && (
                <div className="border-t">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Version</TableHead>
                        <TableHead>Pathname</TableHead>
                        <TableHead>Routing</TableHead>
                        <TableHead>Health</TableHead>
                        <TableHead>Processes</TableHead>
                        <TableHead>
                          <span className="sr-only">Actions</span>
                        </TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {group.versions.map((worker) => {
                        const latest = worker.version === defaultVersion;
                        const runtime = data.metricsStats?.workers?.find(
                          (candidate) =>
                            candidate.name === worker.name &&
                            candidate.version === worker.version,
                        );
                        const isCore = worker.origin !== "user";
                        return (
                          <TableRow key={`${worker.name}@${worker.version}`}>
                            <TableCell>
                              <Badge variant="secondary">
                                <span className="font-mono">
                                  {worker.version}
                                </span>
                              </Badge>
                            </TableCell>
                            <TableCell className="font-mono text-xs">
                              {workerUrl(worker, latest)}
                            </TableCell>
                            <TableCell>
                              {worker.status === "disabled" ? (
                                <span className="text-muted-foreground">
                                  Disabled
                                </span>
                              ) : latest ? (
                                <span className="text-emerald-700">
                                  Default
                                </span>
                              ) : (
                                <span className="text-primary">Enabled</span>
                              )}
                            </TableCell>
                            <TableCell>
                              <Health health={runtime?.health} />
                            </TableCell>
                            <TableCell>
                              {runtime
                                ? `${runtime.activeProcesses ?? 0} active · ${runtime.idleProcesses ?? 0} idle`
                                : "—"}
                            </TableCell>
                            <TableCell>
                              <div className="flex justify-end">
                                <ActionButton
                                  label="Browse files"
                                  onClick={() => onOpen(worker, "files")}
                                >
                                  <FolderOpenIcon />
                                </ActionButton>
                                <ActionButton
                                  label="Open URL"
                                  onClick={() =>
                                    window.open(
                                      workerUrl(worker, latest),
                                      "_blank",
                                      "noopener,noreferrer",
                                    )
                                  }
                                >
                                  <ExternalLinkIcon />
                                </ActionButton>
                                <DropdownMenu>
                                  <DropdownMenuTrigger
                                    render={
                                      <Button
                                        aria-label="Worker actions"
                                        size="icon-sm"
                                        variant="ghost"
                                      />
                                    }
                                  >
                                    <MoreVerticalIcon />
                                  </DropdownMenuTrigger>
                                  <DropdownMenuContent align="end">
                                    <DropdownMenuItem
                                      onClick={() =>
                                        onOpen(worker, "observability")
                                      }
                                    >
                                      <HeartPulseIcon />
                                      Observability
                                    </DropdownMenuItem>
                                    <DropdownMenuItem
                                      onClick={() => onOpen(worker, "logs")}
                                    >
                                      <ScrollTextIcon />
                                      View logs
                                    </DropdownMenuItem>
                                    <DropdownMenuSeparator />
                                    {worker.status === "disabled" ? (
                                      <DropdownMenuItem
                                        onClick={() =>
                                          toggleMutation.mutate({
                                            enable: true,
                                            worker,
                                          })
                                        }
                                      >
                                        <RotateCcwIcon />
                                        Enable version
                                      </DropdownMenuItem>
                                    ) : isCore &&
                                      group.versions.filter(
                                        (candidate) =>
                                          candidate.status !== "disabled",
                                      ).length <= 1 ? (
                                      <DropdownMenuItem disabled>
                                        <ShieldCheckIcon />
                                        Default required
                                      </DropdownMenuItem>
                                    ) : (
                                      <DropdownMenuItem
                                        onClick={() =>
                                          toggleMutation.mutate({
                                            enable: false,
                                            worker,
                                          })
                                        }
                                      >
                                        <PowerOffIcon />
                                        Disable version
                                      </DropdownMenuItem>
                                    )}
                                  </DropdownMenuContent>
                                </DropdownMenu>
                              </div>
                            </TableCell>
                          </TableRow>
                        );
                      })}
                    </TableBody>
                  </Table>
                </div>
              )}
            </Card>
          );
        })}
        {rows.length === 0 && (
          <Card>
            <CardContent className="py-12 text-center text-muted-foreground">
              No workers match the current filters.
            </CardContent>
          </Card>
        )}
      </div>
      <div className="flex items-center justify-between text-sm text-muted-foreground">
        <span>
          Showing {rows.length} of {visible.length} applications
        </span>
        <div className="flex items-center gap-2">
          <Button
            disabled={page === 0}
            onClick={() => setPage((value) => value - 1)}
            size="sm"
            variant="outline"
          >
            Previous
          </Button>
          <span>
            Page {page + 1} of {pages}
          </span>
          <Button
            disabled={page >= pages - 1}
            onClick={() => setPage((value) => value + 1)}
            size="sm"
            variant="outline"
          >
            Next
          </Button>
        </div>
      </div>
      <DeployDialog
        apiKey={apiKey}
        onDeployed={onRefresh}
        open={deployOpen}
        onOpenChange={setDeployOpen}
      />
    </div>
  );
}

function Health({ health }: { health?: RuntimeWorker["health"] }) {
  const status = health?.status ?? "unobserved";
  const color =
    status === "healthy"
      ? "bg-emerald-500"
      : status === "failing"
        ? "bg-destructive"
        : status === "degraded"
          ? "bg-amber-500"
          : "bg-muted-foreground/40";
  return (
    <span className="inline-flex items-center gap-2 text-sm capitalize">
      <span className={`size-2 rounded-full ${color}`} />
      {status}
    </span>
  );
}

function Observability({
  apiKey,
  data,
  target,
}: {
  apiKey: string;
  data: RuntimeData;
  target?: Target;
}) {
  const seriesQuery = useQuery({
    queryKey: ["cpanel", "series", target],
    queryFn: () =>
      apiJson<{
        bucketMs: number;
        partialWindow?: boolean;
        points: Array<{
          durationP95Ms: number | null;
          errorCount: number;
          requestCount: number;
          startMs: number;
        }>;
      }>(
        apiKey,
        `/api/admin/observability/series?windowMs=300000&bucketMs=15000${target ? `&worker=${encodeURIComponent(target.name)}&version=${encodeURIComponent(target.version)}` : ""}`,
      ),
    refetchInterval: 5000,
  });
  const points = seriesQuery.data?.points ?? [];
  const chartData = points.map((point) => ({
    at: new Date(point.startMs).toLocaleTimeString([], {
      minute: "2-digit",
      second: "2-digit",
    }),
    errors: point.errorCount,
    latency: point.durationP95Ms ?? 0,
    requests: point.requestCount,
  }));
  const runtimeWorkers = target
    ? (data.metricsStats?.workers?.filter(
        (worker) =>
          worker.name === target.name && worker.version === target.version,
      ) ?? [])
    : (data.metricsStats?.workers ?? []);
  const active = runtimeWorkers.reduce(
    (sum, worker) => sum + (worker.activeProcesses ?? 0),
    0,
  );
  const idle = runtimeWorkers.reduce(
    (sum, worker) => sum + (worker.idleProcesses ?? 0),
    0,
  );
  const requests = points.reduce((sum, point) => sum + point.requestCount, 0);
  const errors = points.reduce((sum, point) => sum + point.errorCount, 0);
  const p95 =
    [...points].reverse().find((point) => point.durationP95Ms)?.durationP95Ms ??
    0;
  const config = {
    requests: { color: "var(--chart-2)", label: "Requests" },
    errors: { color: "var(--chart-5)", label: "Errors" },
    latency: { color: "var(--chart-4)", label: "P95 latency" },
  } satisfies ChartConfig;
  return (
    <div className="grid gap-4">
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <MetricCard
          description="Five-minute local window"
          icon={GaugeIcon}
          label="Requests"
          value={String(requests)}
        />
        <MetricCard
          description="Latest observed bucket"
          icon={ActivityIcon}
          label="P95 latency"
          value={p95 ? `${p95} ms` : "No data"}
        />
        <MetricCard
          description="Failed dispatches"
          icon={CircleAlertIcon}
          label="Errors"
          value={String(errors)}
        />
        <MetricCard
          description="Current process snapshot"
          icon={CpuIcon}
          label="Processes"
          value={`${active} active · ${idle} idle`}
        />
      </div>
      <Card>
        <CardHeader>
          <CardTitle>Runtime signals</CardTitle>
          <CardDescription>
            Short, bounded series collected by this EdgeR instance.
          </CardDescription>
          <CardAction>
            <Badge variant="outline">Live · 5 minutes</Badge>
          </CardAction>
        </CardHeader>
        <CardContent>
          <ChartContainer className="h-72 w-full" config={config}>
            <LineChart data={chartData}>
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="at"
                tickLine={false}
                axisLine={false}
                minTickGap={24}
              />
              <ChartTooltip content={<ChartTooltipContent />} />
              <Line
                dataKey="requests"
                stroke="var(--color-requests)"
                strokeWidth={2}
                dot={false}
              />
              <Line
                dataKey="errors"
                stroke="var(--color-errors)"
                strokeWidth={2}
                dot={false}
              />
              <Line
                dataKey="latency"
                stroke="var(--color-latency)"
                strokeWidth={2}
                dot={false}
              />
            </LineChart>
          </ChartContainer>
        </CardContent>
      </Card>
    </div>
  );
}

function Logs({ apiKey, target }: { apiKey: string; target?: Target }) {
  const [level, setLevel] = React.useState("all");
  const [query, setQuery] = React.useState("");
  const eventsQuery = useQuery({
    queryKey: ["cpanel", "events", target],
    queryFn: () => {
      const params = new URLSearchParams({ limit: "500" });
      if (target) {
        params.set("worker", target.name);
        params.set("version", target.version);
      }
      return apiJson<{
        events: Array<Record<string, unknown>>;
        stats?: Record<string, number>;
      }>(apiKey, `/api/admin/observability/events?${params}`);
    },
    refetchInterval: 5000,
  });
  const events = (eventsQuery.data?.events ?? []).filter(
    (event) =>
      (level === "all" || event.level === level) &&
      (!query ||
        JSON.stringify(event).toLowerCase().includes(query.toLowerCase())),
  );
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          {target ? `${target.name}@${target.version} logs` : "Runtime logs"}
        </CardTitle>
        <CardDescription>
          Bounded operational events retained by this EdgeR process.
        </CardDescription>
        <CardAction>
          <div className="flex gap-2">
            <InputGroup className="w-56">
              <InputGroupAddon>
                <SearchIcon />
              </InputGroupAddon>
              <InputGroupInput
                aria-label="Search logs"
                value={query}
                onChange={(event) => setQuery(event.currentTarget.value)}
              />
            </InputGroup>
            <Select
              value={level}
              onValueChange={(value) => setLevel(value ?? "all")}
            >
              <SelectTrigger className="w-32">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {["all", "info", "warn", "error"].map((value) => (
                    <SelectItem key={value} value={value}>
                      {value}
                    </SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
          </div>
        </CardAction>
      </CardHeader>
      <CardContent>
        <div className="max-h-[calc(100vh-18rem)] overflow-auto rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Time</TableHead>
                <TableHead>Level</TableHead>
                <TableHead>Worker</TableHead>
                <TableHead>Event</TableHead>
                <TableHead>Outcome</TableHead>
                <TableHead>Duration</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {events.map((event) => (
                <TableRow key={String(event.id)}>
                  <TableCell className="whitespace-nowrap text-muted-foreground">
                    {new Date(Number(event.atMs)).toLocaleTimeString()}
                  </TableCell>
                  <TableCell>
                    <Badge
                      variant={
                        event.level === "error" ? "destructive" : "secondary"
                      }
                    >
                      {String(event.level ?? "info")}
                    </Badge>
                  </TableCell>
                  <TableCell className="font-mono text-xs">
                    {event.worker
                      ? `${event.worker}@${event.version}`
                      : "runtime"}
                  </TableCell>
                  <TableCell>
                    {String(event.kind ?? event.source ?? "-")}
                  </TableCell>
                  <TableCell>
                    {String(event.outcome ?? event.status ?? "-")}
                  </TableCell>
                  <TableCell>
                    {event.durationMs == null ? "-" : `${event.durationMs} ms`}
                  </TableCell>
                </TableRow>
              ))}
              {events.length === 0 && (
                <TableRow>
                  <TableCell
                    className="h-32 text-center text-muted-foreground"
                    colSpan={6}
                  >
                    No events match the current filters.
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      </CardContent>
    </Card>
  );
}

function Files({
  apiKey,
  path,
  setPath,
  target,
}: {
  apiKey: string;
  path: string;
  setPath(path: string): void;
  target: Target;
}) {
  const queryClient = useQueryClient();
  const fileInput = React.useRef<HTMLInputElement>(null);
  const filesQuery = useQuery({
    queryKey: ["cpanel", "files", target, path],
    queryFn: () =>
      apiJson<{
        entries: Array<{ kind: "dir" | "file"; name: string; size: number }>;
      }>(
        apiKey,
        `/api/admin/workers/${encodeURIComponent(target.name)}/files?${new URLSearchParams({ path, version: target.version })}`,
      ),
  });
  const upload = useMutation({
    mutationFn: (body: Uint8Array) =>
      apiJson(
        apiKey,
        `/api/admin/workers/${encodeURIComponent(target.name)}/files?${new URLSearchParams({ path, version: target.version })}`,
        {
          body: bytesBody(body),
          headers: { "content-type": "application/zip" },
          method: "POST",
        },
      ),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["cpanel", "files", target] }),
  });
  async function pick(event: React.ChangeEvent<HTMLInputElement>) {
    const files = [...(event.currentTarget.files ?? [])];
    const map: Record<string, Uint8Array> = {};
    for (const file of files)
      map[file.webkitRelativePath || file.name] = new Uint8Array(
        await file.arrayBuffer(),
      );
    if (files.length) upload.mutate(zipSync(map));
    event.currentTarget.value = "";
  }
  const crumbs = path ? path.split("/") : [];
  return (
    <Card>
      <CardHeader>
        <CardTitle>Files</CardTitle>
        <CardDescription>
          Browse and publish files for {target.name}@{target.version}.
        </CardDescription>
        <CardAction>
          <Button onClick={() => fileInput.current?.click()} variant="outline">
            <UploadIcon />
            Upload files
          </Button>
          <Input
            className="hidden"
            multiple
            onChange={(event) => void pick(event)}
            ref={fileInput}
            type="file"
          />
        </CardAction>
      </CardHeader>
      <CardContent>
        <div className="mb-3 flex items-center gap-1 text-sm text-muted-foreground">
          <button
            className="hover:text-foreground"
            onClick={() => setPath("")}
            type="button"
          >
            {target.name}
          </button>
          {crumbs.map((crumb, index) => (
            <React.Fragment key={`${crumb}-${index}`}>
              <span>/</span>
              <button
                className="font-mono hover:text-foreground"
                onClick={() => setPath(crumbs.slice(0, index + 1).join("/"))}
                type="button"
              >
                {crumb}
              </button>
            </React.Fragment>
          ))}
        </div>
        {upload.error && (
          <p className="mb-3 text-sm text-destructive">
            {upload.error.message}
          </p>
        )}
        <div className="overflow-hidden rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead className="text-right">Size</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {path && (
                <TableRow
                  className="cursor-pointer"
                  onClick={() => setPath(crumbs.slice(0, -1).join("/"))}
                >
                  <TableCell colSpan={2}>
                    <FolderOpenIcon className="mr-2 inline size-4" />
                    ..
                  </TableCell>
                </TableRow>
              )}
              {filesQuery.data?.entries.map((entry) => (
                <TableRow
                  className={entry.kind === "dir" ? "cursor-pointer" : ""}
                  key={entry.name}
                  onClick={
                    entry.kind === "dir"
                      ? () =>
                          setPath(path ? `${path}/${entry.name}` : entry.name)
                      : undefined
                  }
                >
                  <TableCell>
                    <span className="flex items-center gap-2 font-mono text-sm">
                      {entry.kind === "dir" ? (
                        <FolderIcon className="size-4 text-primary" />
                      ) : (
                        <FileIcon className="size-4 text-muted-foreground" />
                      )}
                      {entry.name}
                    </span>
                  </TableCell>
                  <TableCell className="text-right text-muted-foreground">
                    {entry.kind === "dir" ? "—" : formatBytes(entry.size)}
                  </TableCell>
                </TableRow>
              ))}
              {!filesQuery.isLoading && !filesQuery.data?.entries.length && (
                <TableRow>
                  <TableCell
                    className="h-32 text-center text-muted-foreground"
                    colSpan={2}
                  >
                    This folder is empty.
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      </CardContent>
    </Card>
  );
}

function DeployDialog({
  apiKey,
  onDeployed,
  onOpenChange,
  open,
}: {
  apiKey: string;
  onDeployed(): Promise<void>;
  onOpenChange(open: boolean): void;
  open: boolean;
}) {
  const [stage, setStage] = React.useState<{
    error?: string;
    file?: File;
    preview?: {
      entrypoint: string;
      files: string[];
      name: string;
      version: string;
    };
    zip?: Uint8Array;
  }>({});
  const input = React.useRef<HTMLInputElement>(null);
  async function stageFile(file: File) {
    try {
      const zip = new Uint8Array(await file.arrayBuffer());
      const files = unzipSync(zip);
      const names = Object.keys(files);
      const manifestName = names.find((name) => name.endsWith("manifest.yaml"));
      if (!manifestName)
        throw new Error("The archive must contain manifest.yaml");
      const manifest = new TextDecoder().decode(files[manifestName]);
      const read = (key: string) =>
        manifest
          .match(new RegExp(`^${key}:\\s*["']?([^"'\\n]+)`, "m"))?.[1]
          ?.trim();
      const name = read("name");
      const version = read("version");
      const entrypoint = read("entrypoint");
      if (!name || !version || !entrypoint)
        throw new Error(
          "manifest.yaml must define name, version, and entrypoint",
        );
      setStage({
        file,
        preview: { entrypoint, files: names, name, version },
        zip,
      });
    } catch (reason) {
      setStage({
        error: reason instanceof Error ? reason.message : String(reason),
      });
    }
  }
  const deploy = useMutation({
    mutationFn: () =>
      apiJson<{ name: string; url: string; version: string }>(
        apiKey,
        "/api/admin/workers/install",
        {
          body: stage.zip ? bytesBody(stage.zip) : undefined,
          headers: { "content-type": "application/zip" },
          method: "POST",
        },
      ),
    onSuccess: async () => {
      await onDeployed();
      setStage({});
      onOpenChange(false);
    },
    onError: (reason) =>
      setStage((current) => ({ ...current, error: reason.message })),
  });
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setStage({});
        onOpenChange(next);
      }}
    >
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Deploy an app</DialogTitle>
          <DialogDescription>
            Choose a zip package. EdgeR validates and activates it without a
            runtime restart.
          </DialogDescription>
        </DialogHeader>
        <button
          className="grid min-h-44 place-items-center rounded-xl border-2 border-dashed p-6 text-center transition-colors hover:bg-accent/50"
          onClick={() => input.current?.click()}
          type="button"
        >
          <span>
            <UploadCloudIcon className="mx-auto mb-2 size-8 text-muted-foreground" />
            <strong className="block text-sm">Choose a zip package</strong>
            <small className="text-muted-foreground">
              Limit: 4 MiB · manifest.yaml required
            </small>
          </span>
          <Input
            accept=".zip"
            className="hidden"
            ref={input}
            type="file"
            onChange={(event) => {
              const file = event.currentTarget.files?.[0];
              if (file) void stageFile(file);
            }}
          />
        </button>
        {stage.error && (
          <p className="rounded-lg border border-destructive/30 bg-destructive/10 p-3 text-sm text-destructive">
            {stage.error}
          </p>
        )}
        {stage.preview && (
          <Card>
            <CardContent className="grid gap-2 p-4">
              <div className="flex items-center gap-2">
                <FileArchiveIcon className="size-5 text-primary" />
                <strong>
                  {stage.preview.name}@{stage.preview.version}
                </strong>
              </div>
              <p className="text-sm text-muted-foreground">
                Entrypoint {stage.preview.entrypoint} ·{" "}
                {stage.preview.files.length} files
              </p>
            </CardContent>
          </Card>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            disabled={!stage.zip || deploy.isPending}
            onClick={() => deploy.mutate()}
          >
            <RocketIcon />
            {deploy.isPending ? "Deploying…" : "Deploy"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function Shell({
  apiKey,
  data,
  logout,
}: {
  apiKey: string;
  data: RuntimeData;
  logout(): void;
}) {
  const queryClient = useQueryClient();
  const [route, setRoute] = React.useState(readRoute);
  const active =
    NAVIGATION.find((entry) => entry.id === route.view) ??
    NAVIGATION.find(
      (entry) => entry.id === (route.target ? "workers" : "observability"),
    ) ??
    NAVIGATION[0];
  React.useEffect(() => {
    const pop = () => setRoute(readRoute());
    addEventListener("popstate", pop);
    return () => removeEventListener("popstate", pop);
  }, []);
  function navigate(next: RouteState) {
    history.pushState(null, "", routePath(next));
    setRoute(next);
  }
  const refresh = async () => {
    await apiJson(apiKey, "/api/admin/workers/rescan", {
      body: JSON.stringify({ dryRun: false }),
      headers: { "content-type": "application/json" },
      method: "POST",
    }).catch(() => undefined);
    await queryClient.invalidateQueries({ queryKey: ["cpanel"] });
  };
  const targetWorker = route.target
    ? data.workers.find(
        (worker) =>
          worker.name === route.target?.name &&
          worker.version === route.target?.version,
      )
    : undefined;
  const title = route.target ? route.target.name : active.title;
  const description = route.target
    ? `Version ${route.target.version} · ${route.view}`
    : active.description;
  return (
    <SidebarProvider className="h-screen min-h-0">
      <Sidebar>
        <SidebarHeader>
          <div className="flex items-center gap-2 px-2 py-2">
            <CpuIcon className="size-5" />
            <span>
              <strong className="block text-sm">EdgeR</strong>
              <small className="text-xs text-muted-foreground">cPanel</small>
            </span>
          </div>
        </SidebarHeader>
        <SidebarContent>
          <SidebarMenu>
            {NAVIGATION.map((entry) => (
              <SidebarMenuItem key={entry.id}>
                <SidebarMenuButton
                  isActive={
                    route.target
                      ? entry.id === "workers"
                      : route.view === entry.id ||
                        (route.view === "logs" && entry.id === "observability")
                  }
                  onClick={() => navigate({ path: "", view: entry.id })}
                >
                  <entry.icon />
                  <span>{entry.title}</span>
                </SidebarMenuButton>
              </SidebarMenuItem>
            ))}
          </SidebarMenu>
        </SidebarContent>
        <SidebarFooter>
          <div className="flex items-center gap-2 px-2 py-2">
            <span className="grid size-8 place-items-center rounded-full bg-muted text-xs font-medium">
              {(data.principal.name ?? "?").slice(0, 2)}
            </span>
            <span className="min-w-0 flex-1">
              <strong className="block truncate text-xs">
                {data.principal.name}
              </strong>
              <small className="block truncate text-xs text-muted-foreground">
                {data.principal.role}
              </small>
            </span>
            <ActionButton label="Disconnect" onClick={logout}>
              <LogOutIcon />
            </ActionButton>
          </div>
        </SidebarFooter>
      </Sidebar>
      <SidebarInset className="min-w-0">
        <header className="flex min-h-16 items-center gap-3 border-b px-4 sm:px-6">
          {route.target && (
            <ActionButton
              label="Back to workers"
              onClick={() => navigate({ path: "", view: "workers" })}
            >
              <ArrowLeftIcon />
            </ActionButton>
          )}
          <div className="min-w-0">
            <h1 className="truncate font-heading text-lg font-medium">
              {title}
            </h1>
            <p className="truncate text-sm text-muted-foreground">
              {description}
            </p>
          </div>
          <Button
            className="ml-auto"
            onClick={() => void refresh()}
            variant="outline"
          >
            <RefreshCwIcon />
            Refresh
          </Button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto p-4 sm:p-6">
          {route.target && (
            <Tabs
              className="mb-4"
              value={route.view}
              onValueChange={(value) =>
                navigate({
                  path: value === "files" ? route.path : "",
                  target: route.target,
                  view: value as View,
                })
              }
            >
              <TabsList>
                <TabsTrigger value="files">Files</TabsTrigger>
                <TabsTrigger value="observability">Observability</TabsTrigger>
                <TabsTrigger value="logs">Logs</TabsTrigger>
              </TabsList>
            </Tabs>
          )}
          {!route.target && ["observability", "logs"].includes(route.view) && (
            <Tabs
              className="mb-4"
              value={route.view}
              onValueChange={(value) =>
                navigate({ path: "", view: value as View })
              }
            >
              <TabsList>
                <TabsTrigger value="observability">Overview</TabsTrigger>
                <TabsTrigger value="logs">Logs</TabsTrigger>
              </TabsList>
            </Tabs>
          )}
          {route.view === "overview" && (
            <Overview
              data={data}
              onWorkers={() => navigate({ path: "", view: "workers" })}
            />
          )}
          {route.view === "workers" && (
            <Workers
              apiKey={apiKey}
              data={data}
              onOpen={(worker, view) =>
                navigate({
                  path: "",
                  target: { name: worker.name, version: worker.version },
                  view,
                })
              }
              onRefresh={refresh}
            />
          )}
          {route.view === "observability" && (
            <Observability apiKey={apiKey} data={data} target={route.target} />
          )}
          {route.view === "logs" && (
            <Logs apiKey={apiKey} target={route.target} />
          )}
          {route.view === "files" && route.target && targetWorker && (
            <Files
              apiKey={apiKey}
              path={route.path}
              setPath={(path) => navigate({ ...route, path })}
              target={route.target}
            />
          )}
        </div>
      </SidebarInset>
    </SidebarProvider>
  );
}

function CpanelApp() {
  const [apiKey, setApiKey] = React.useState(
    () => sessionStorage.getItem(SESSION_KEY) ?? "",
  );
  const runtimeQuery = useQuery({
    queryKey: ["cpanel", "runtime", apiKey],
    queryFn: () => loadAll(apiKey),
    enabled: Boolean(apiKey),
    refetchOnWindowFocus: true,
  });
  React.useEffect(() => {
    if (runtimeQuery.error && apiKey) {
      sessionStorage.removeItem(SESSION_KEY);
      setApiKey("");
    }
  }, [apiKey, runtimeQuery.error]);
  if (!apiKey) return <Login onAuthenticated={setApiKey} />;
  if (runtimeQuery.isLoading || !runtimeQuery.data)
    return (
      <div className="grid min-h-screen place-items-center text-muted-foreground">
        Restoring session…
      </div>
    );
  return (
    <Shell
      apiKey={apiKey}
      data={runtimeQuery.data}
      logout={() => {
        sessionStorage.removeItem(SESSION_KEY);
        setApiKey("");
      }}
    />
  );
}

const rootRoute = createRootRoute({ component: CpanelApp });
const router = createRouter({ routeTree: rootRoute, basepath: "/cpanel" });
declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
const queryClient = new QueryClient({
  defaultOptions: { queries: { retry: 1, staleTime: 10_000 } },
});
const root = document.getElementById("root");
if (!root) throw new Error("root element not found");
createRoot(root).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <TooltipProvider delay={200}>
          <RouterProvider router={router} />
        </TooltipProvider>
      </ThemeProvider>
    </QueryClientProvider>
  </React.StrictMode>,
);
