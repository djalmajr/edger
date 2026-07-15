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
import {
  type ColumnDef,
  getCoreRowModel,
  getPaginationRowModel,
  getSortedRowModel,
  type SortingState,
  useReactTable,
} from "@tanstack/react-table";
import * as React from "react";
import { createPortal } from "react-dom";
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
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
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
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@edger/ui/components/ui/sheet";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
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
  ChevronDownIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  CopyIcon,
  CpuIcon,
  DownloadIcon,
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
  MonitorIcon,
  MoreVerticalIcon,
  MoonIcon,
  PowerOffIcon,
  RefreshCwIcon,
  RocketIcon,
  RotateCcwIcon,
  ScrollTextIcon,
  SearchIcon,
  ShieldCheckIcon,
  SunIcon,
  UploadCloudIcon,
  UploadIcon,
} from "@edger/ui/icons/lucide";
import {
  ThemeProvider,
  type ThemePreference,
  useTheme,
} from "@edger/ui/lib/theme";

import "./app.css";
import {
  DataGrid,
  DataGridColumnHeader,
  DEFAULT_PAGE_SIZE,
  PaginationControls,
} from "./components/data-grid";
import { Overview } from "./components/overview";
import {
  I18nProvider,
  type Locale,
  type TranslationKey,
  useI18n,
} from "./lib/i18n";
import {
  apiDownload,
  apiJson,
  compareSemver,
  kindLabel,
  loadAll,
  type OperationalEvent,
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
    descriptionKey: "nav.overview.description" as TranslationKey,
    icon: GaugeIcon,
    id: "overview" as const,
    titleKey: "nav.overview" as TranslationKey,
  },
  {
    descriptionKey: "nav.workers.description" as TranslationKey,
    icon: CpuIcon,
    id: "workers" as const,
    titleKey: "nav.workers" as TranslationKey,
  },
  {
    descriptionKey: "nav.observability.description" as TranslationKey,
    icon: ActivityIcon,
    id: "observability" as const,
    titleKey: "nav.observability" as TranslationKey,
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

const PageActionsContext = React.createContext<HTMLElement | null>(null);

function PageActions({ children }: { children: React.ReactNode }) {
  const target = React.useContext(PageActionsContext);
  return target ? createPortal(children, target) : null;
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
  const [pageSize, setPageSize] = React.useState(DEFAULT_PAGE_SIZE);
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
  const pages = Math.max(1, Math.ceil(visible.length / pageSize));
  const rows = visible.slice(page * pageSize, page * pageSize + pageSize);
  React.useEffect(() => {
    if (page >= pages) setPage(pages - 1);
  }, [page, pages]);
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
      <PageActions>
        <Button onClick={() => setDeployOpen(true)}>
          <UploadCloudIcon />
          Deploy app
        </Button>
      </PageActions>
      <div className="flex flex-wrap gap-2">
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
          onValueChange={(value) => {
            setKind(value ?? "all");
            setPage(0);
          }}
        >
          <SelectTrigger aria-label="Filter by worker kind" className="w-44">
            <SelectValue>{kind === "all" ? "All kinds" : kind}</SelectValue>
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
            <Card className="gap-0 py-0" key={group.name}>
              <button
                className="flex w-full items-center gap-3 p-3 text-left"
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
                        const canOpenUrl =
                          worker.status !== "disabled" &&
                          worker.name !== "cpanel";
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
                                  disabled={!canOpenUrl}
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
                                  <DropdownMenuContent
                                    align="end"
                                    className="min-w-44 whitespace-nowrap"
                                  >
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
      <div className="flex flex-wrap items-center justify-between gap-3 text-sm text-muted-foreground">
        <span>
          Showing {Math.min((page + 1) * pageSize, visible.length)} of{" "}
          {visible.length} applications
        </span>
        <nav aria-label="Workers pagination">
          <PaginationControls
            canNextPage={page < pages - 1}
            canPreviousPage={page > 0}
            onFirstPage={() => setPage(0)}
            onLastPage={() => setPage(pages - 1)}
            onNextPage={() => setPage((value) => value + 1)}
            onPageSizeChange={(value) => {
              setPageSize(value);
              setPage(0);
            }}
            onPreviousPage={() => setPage((value) => value - 1)}
            pageCount={pages}
            pageIndex={page}
            pageSize={pageSize}
          />
        </nav>
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
  const [selectedEvent, setSelectedEvent] =
    React.useState<OperationalEvent | null>(null);
  const [sorting, setSorting] = React.useState<SortingState>([
    { desc: true, id: "atMs" },
  ]);
  const levelLabels: Record<string, string> = {
    all: "All levels",
    error: "Error",
    info: "Info",
    warn: "Warning",
  };
  const eventsQuery = useQuery({
    queryKey: ["cpanel", "events", target],
    queryFn: () => {
      const params = new URLSearchParams({ limit: "500" });
      if (target) {
        params.set("worker", target.name);
        params.set("version", target.version);
      }
      return apiJson<{
        events: OperationalEvent[];
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
  const columns = React.useMemo<ColumnDef<OperationalEvent>[]>(
    () => [
      {
        accessorKey: "atMs",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label="Time" />
        ),
        cell: ({ row }) => (
          <span className="text-muted-foreground">
            {new Date(Number(row.original.atMs)).toLocaleTimeString()}
          </span>
        ),
      },
      {
        accessorKey: "level",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label="Level" />
        ),
        cell: ({ row }) => (
          <Badge
            className={
              row.original.level === "error"
                ? "border-rose-200 bg-rose-50 text-rose-700 dark:border-rose-800 dark:bg-rose-950/40 dark:text-rose-300"
                : row.original.level === "warn"
                  ? "border-amber-200 bg-amber-50 text-amber-700 dark:border-amber-800 dark:bg-amber-950/40 dark:text-amber-300"
                  : "border-sky-200 bg-sky-50 text-sky-700 dark:border-sky-800 dark:bg-sky-950/40 dark:text-sky-300"
            }
            variant="outline"
          >
            {row.original.level ?? "info"}
          </Badge>
        ),
      },
      {
        accessorFn: (event) =>
          event.worker ? `${event.worker}@${event.version}` : "runtime",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label="Worker" />
        ),
        id: "worker",
        cell: ({ getValue }) => (
          <span className="font-mono text-xs">{String(getValue())}</span>
        ),
      },
      {
        accessorFn: (event) => event.kind ?? event.source ?? "-",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label="Event" />
        ),
        id: "event",
      },
      {
        accessorFn: (event) => event.outcome ?? event.status ?? "-",
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label="Outcome" />
        ),
        id: "outcome",
      },
      {
        accessorFn: (event) => event.durationMs,
        header: ({ column }) => (
          <DataGridColumnHeader column={column} label="Duration" />
        ),
        id: "durationMs",
        cell: ({ row }) =>
          row.original.durationMs == null
            ? "-"
            : `${row.original.durationMs} ms`,
      },
    ],
    [],
  );
  const table = useReactTable({
    autoResetPageIndex: false,
    columns,
    data: events,
    getCoreRowModel: getCoreRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    getSortedRowModel: getSortedRowModel(),
    initialState: {
      pagination: { pageIndex: 0, pageSize: DEFAULT_PAGE_SIZE },
    },
    onSortingChange: setSorting,
    state: { sorting },
  });
  React.useEffect(() => {
    table.setPageIndex(0);
  }, [level, query, table]);
  const detailFields = selectedEvent
    ? [
        ["ID", selectedEvent.id],
        [
          "Timestamp",
          selectedEvent.atMs == null
            ? undefined
            : new Date(selectedEvent.atMs).toLocaleString(),
        ],
        ["Source", selectedEvent.source],
        ["Level", selectedEvent.level],
        ["Namespace", selectedEvent.namespace],
        ["Worker", selectedEvent.worker],
        ["Version", selectedEvent.version],
        ["Event", selectedEvent.kind],
        ["Outcome", selectedEvent.outcome],
        ["Status", selectedEvent.status],
        [
          "Duration",
          selectedEvent.durationMs == null
            ? undefined
            : `${selectedEvent.durationMs} ms`,
        ],
        ["Process ID", selectedEvent.processId],
        ["Request ID", selectedEvent.requestId],
        ["Trace ID", selectedEvent.traceId],
        ["Code", selectedEvent.code],
        ["Message", selectedEvent.message],
        ["Truncated", selectedEvent.truncated],
        ["Dropped count", selectedEvent.droppedCount],
      ].filter(([, value]) => value !== undefined && value !== null)
    : [];
  return (
    <>
      <Card className="gap-3 overflow-visible rounded-none bg-transparent py-0 ring-0">
      <CardHeader className="px-0">
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
              <SelectTrigger aria-label="Filter by log level" className="w-32">
                <SelectValue>{levelLabels[level]}</SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectGroup>
                  {["all", "info", "warn", "error"].map((value) => (
                    <SelectItem key={value} value={value}>
                      {levelLabels[value]}
                    </SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
          </div>
        </CardAction>
      </CardHeader>
      <CardContent className="px-0">
        <DataGrid
          emptyText="No events match the current filters."
          onRowClick={setSelectedEvent}
          rowLabel={(event) =>
            `View details for ${event.kind ?? event.source ?? "event"}`
          }
          table={table}
        />
      </CardContent>
      </Card>
      <Sheet
        onOpenChange={(open) => {
          if (!open) setSelectedEvent(null);
        }}
        open={selectedEvent !== null}
      >
        <SheetContent className="data-[side=right]:w-full data-[side=right]:sm:max-w-2xl">
          <SheetHeader>
            <SheetTitle>Event details</SheetTitle>
            <SheetDescription>
              Complete allowlisted payload retained by this EdgeR process.
            </SheetDescription>
          </SheetHeader>
          {selectedEvent && (
            <div className="grid gap-4 overflow-y-auto px-4 pb-4">
              <dl className="grid gap-3">
                {detailFields.map(([label, value]) => (
                  <div
                    className="grid gap-1 border-b pb-3 last:border-0"
                    key={String(label)}
                  >
                    <dt className="font-medium text-muted-foreground text-xs uppercase tracking-wide">
                      {String(label)}
                    </dt>
                    <dd className="break-words font-mono text-sm">
                      {String(value)}
                    </dd>
                  </div>
                ))}
              </dl>
              <div className="grid gap-2">
                <div className="flex items-center justify-between">
                  <strong className="text-sm">JSON</strong>
                  <Button
                    onClick={() =>
                      void navigator.clipboard.writeText(
                        JSON.stringify(selectedEvent, null, 2),
                      )
                    }
                    size="sm"
                    variant="outline"
                  >
                    <CopyIcon />
                    Copy JSON
                  </Button>
                </div>
                <pre className="overflow-x-auto rounded-md bg-muted p-3 text-xs">
                  {JSON.stringify(selectedEvent, null, 2)}
                </pre>
              </div>
            </div>
          )}
        </SheetContent>
      </Sheet>
    </>
  );
}

function Files({
  apiKey,
  mutable,
  path,
  setPath,
  target,
}: {
  apiKey: string;
  mutable: boolean;
  path: string;
  setPath(path: string): void;
  target: Target;
}) {
  const queryClient = useQueryClient();
  const fileInput = React.useRef<HTMLInputElement>(null);
  const [downloadError, setDownloadError] = React.useState("");
  const [downloading, setDownloading] = React.useState("");
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
  async function downloadEntry(entry: { kind: "dir" | "file"; name: string }) {
    const entryPath = path ? `${path}/${entry.name}` : entry.name;
    setDownloadError("");
    setDownloading(entryPath);
    try {
      const { blob, filename } = await apiDownload(
        apiKey,
        `/api/admin/workers/${encodeURIComponent(target.name)}/files/download?${new URLSearchParams({ path: entryPath, version: target.version })}`,
      );
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      link.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 0);
    } catch (error) {
      setDownloadError(error instanceof Error ? error.message : String(error));
    } finally {
      setDownloading("");
    }
  }
  const crumbs = path ? path.split("/") : [];
  return (
    <>
      {mutable && (
        <PageActions>
          <Button
            onClick={() => fileInput.current?.click()}
            variant="outline"
          >
            <UploadIcon />
            Upload files
          </Button>
        </PageActions>
      )}
      <Input
        className="hidden"
        multiple
        onChange={(event) => void pick(event)}
        ref={fileInput}
        type="file"
      />
      <Card className="gap-3 overflow-visible rounded-none bg-transparent py-0 ring-0">
        <CardContent className="px-0">
          {path && (
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
                    onClick={() =>
                      setPath(crumbs.slice(0, index + 1).join("/"))
                    }
                    type="button"
                  >
                    {crumb}
                  </button>
                </React.Fragment>
              ))}
            </div>
          )}
        {upload.error && (
          <p className="mb-3 text-sm text-destructive">
            {upload.error.message}
          </p>
        )}
        {downloadError && (
          <p className="mb-3 text-sm text-destructive">{downloadError}</p>
        )}
        <div className="overflow-hidden rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead className="text-right">Size</TableHead>
                <TableHead className="w-16 text-right">Actions</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {path && (
                <TableRow
                  className="cursor-pointer"
                  onClick={() => setPath(crumbs.slice(0, -1).join("/"))}
                >
                  <TableCell colSpan={3}>
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
                  <TableCell className="text-right">
                    <ActionButton
                      label={`Download ${entry.name}`}
                      disabled={
                        downloading ===
                        (path ? `${path}/${entry.name}` : entry.name)
                      }
                      onClick={(event) => {
                        event.stopPropagation();
                        void downloadEntry(entry);
                      }}
                    >
                      <DownloadIcon />
                    </ActionButton>
                  </TableCell>
                </TableRow>
              ))}
              {!filesQuery.isLoading && !filesQuery.data?.entries.length && (
                <TableRow>
                  <TableCell
                    className="h-32 text-center text-muted-foreground"
                    colSpan={3}
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
    </>
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

const LOCALE_OPTIONS: Array<{
  flag: string;
  label: string;
  value: Locale;
}> = [
  { flag: "🇧🇷", label: "Português", value: "pt-BR" },
  { flag: "🇺🇸", label: "English", value: "en-US" },
  { flag: "🇪🇸", label: "Español", value: "es-ES" },
];

function LanguageMenu() {
  const { locale, setLocale, t } = useI18n();
  const selected =
    LOCALE_OPTIONS.find((option) => option.value === locale) ??
    LOCALE_OPTIONS[0];
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            aria-label={t("preferences.language")}
            size="icon-sm"
            title={t("preferences.language")}
            variant="ghost"
          />
        }
      >
        <span aria-hidden className="text-lg leading-none">
          {selected.flag}
        </span>
      </DropdownMenuTrigger>
      <DropdownMenuContent className="min-w-40" side="bottom">
        <DropdownMenuRadioGroup
          onValueChange={(value) => setLocale(value as Locale)}
          value={locale}
        >
          {LOCALE_OPTIONS.map((option) => (
            <DropdownMenuRadioItem key={option.value} value={option.value}>
              <span aria-hidden className="text-base leading-none">
                {option.flag}
              </span>
              {option.label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function ThemeMenu() {
  const { t } = useI18n();
  const { resolvedTheme, setTheme, theme } = useTheme();
  const options: Array<{ label: string; value: ThemePreference }> = [
    { label: t("preferences.theme.light"), value: "light" },
    { label: t("preferences.theme.dark"), value: "dark" },
    { label: t("preferences.theme.system"), value: "system" },
  ];
  const currentLabel =
    options.find((option) => option.value === theme)?.label ?? options[2].label;
  const ThemeIcon =
    theme === "system"
      ? MonitorIcon
      : resolvedTheme === "dark"
        ? MoonIcon
        : SunIcon;
  const label = `${t("preferences.theme")}: ${currentLabel}`;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            aria-label={label}
            size="icon-sm"
            title={label}
            variant="ghost"
          />
        }
      >
        <ThemeIcon />
      </DropdownMenuTrigger>
      <DropdownMenuContent className="min-w-32" side="bottom">
        <DropdownMenuRadioGroup
          onValueChange={(value) => setTheme(value as ThemePreference)}
          value={theme}
        >
          {options.map((option) => (
            <DropdownMenuRadioItem key={option.value} value={option.value}>
              {option.label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function AccountMenu({
  logout,
  principal,
}: {
  logout(): void;
  principal: RuntimeData["principal"];
}) {
  const { t } = useI18n();
  const name = principal.name ?? "—";
  const initials = name.slice(0, 2).toLocaleUpperCase();
  const namespaces = principal.namespaces?.join(", ") || "*";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <button
            aria-label={t("account.label")}
            className="grid size-8 shrink-0 place-items-center rounded-full bg-emerald-600 text-xs font-semibold text-white outline-none transition-opacity hover:opacity-90 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2"
            title={name}
            type="button"
          />
        }
      >
        {initials}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-56" side="bottom">
        <DropdownMenuGroup>
          <DropdownMenuLabel className="font-normal">
            <div className="flex items-center gap-3 py-1">
              <span className="grid size-9 shrink-0 place-items-center rounded-full bg-emerald-600 text-xs font-semibold text-white">
                {initials}
              </span>
              <div className="min-w-0">
                <strong className="block truncate text-sm">{name}</strong>
                <span className="block truncate text-xs text-muted-foreground">
                  {t("account.role")}: {principal.role ?? "—"}
                </span>
                <span className="block truncate text-xs text-muted-foreground">
                  {t("account.namespaces")}: {namespaces}
                </span>
              </div>
            </div>
          </DropdownMenuLabel>
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={logout} variant="destructive">
          <LogOutIcon />
          {t("account.logout")}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
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
  const { t } = useI18n();
  const queryClient = useQueryClient();
  const [pageActionsElement, setPageActionsElement] =
    React.useState<HTMLDivElement | null>(null);
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
  const title = route.target ? route.target.name : t(active.titleKey);
  const description = route.target
    ? `Version ${route.target.version} · ${route.view}`
    : t(active.descriptionKey);
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
          <SidebarGroup>
            <SidebarMenu>
              {NAVIGATION.map((entry) => (
                <SidebarMenuItem key={entry.id}>
                  <SidebarMenuButton
                    isActive={
                      route.target
                        ? entry.id === "workers"
                        : route.view === entry.id ||
                          (route.view === "logs" &&
                            entry.id === "observability")
                    }
                    onClick={() => navigate({ path: "", view: entry.id })}
                  >
                    <entry.icon />
                    <span>{t(entry.titleKey)}</span>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              ))}
            </SidebarMenu>
          </SidebarGroup>
        </SidebarContent>
      </Sidebar>
      <SidebarInset className="min-w-0">
        <header className="flex min-h-16 items-center gap-3 border-b px-4">
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
          <div
            className="ml-auto flex items-center gap-1"
            data-slot="header-preferences"
          >
            <LanguageMenu />
            <ThemeMenu />
            <AccountMenu logout={logout} principal={data.principal} />
          </div>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          <PageActionsContext.Provider value={pageActionsElement}>
            <div className="mb-4 flex min-h-8 items-center gap-3">
              {route.target && (
                <Tabs
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
                    <TabsTrigger value="observability">
                      Observability
                    </TabsTrigger>
                    <TabsTrigger value="logs">Logs</TabsTrigger>
                  </TabsList>
                </Tabs>
              )}
              {!route.target &&
                ["observability", "logs"].includes(route.view) && (
                  <Tabs
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
              <div
                className="ml-auto flex items-center gap-2"
                data-slot="page-actions"
              >
                <Button onClick={() => void refresh()} variant="outline">
                  <RefreshCwIcon />
                  Refresh
                </Button>
                <div className="contents" ref={setPageActionsElement} />
              </div>
            </div>
            {route.view === "overview" && (
              <Overview
                apiKey={apiKey}
                data={data}
                onLogs={() => navigate({ path: "", view: "logs" })}
                onWorker={(name, version) =>
                  navigate({
                    path: "",
                    target: { name, version },
                    view: "observability",
                  })
                }
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
              <Observability
                apiKey={apiKey}
                data={data}
                target={route.target}
              />
            )}
            {route.view === "logs" && (
              <Logs apiKey={apiKey} target={route.target} />
            )}
            {route.view === "files" && route.target && targetWorker && (
              <Files
                apiKey={apiKey}
                mutable={targetWorker.origin === "user"}
                path={route.path}
                setPath={(path) => navigate({ ...route, path })}
                target={route.target}
              />
            )}
          </PageActionsContext.Provider>
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
    refetchInterval: 5000,
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
        <I18nProvider>
          <TooltipProvider delay={200}>
            <RouterProvider router={router} />
          </TooltipProvider>
        </I18nProvider>
      </ThemeProvider>
    </QueryClientProvider>
  </React.StrictMode>,
);
