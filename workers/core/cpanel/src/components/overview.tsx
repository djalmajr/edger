import { useQuery } from "@tanstack/react-query";

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
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@edger/ui/components/ui/table";
import {
  ActivityIcon,
  BoxIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  CircleCheckIcon,
  CpuIcon,
  ListIcon,
  RouteIcon,
} from "@edger/ui/icons/lucide";

import {
  apiJson,
  type OperationalEvent,
  type RuntimeData,
  type RuntimeWorker,
} from "../lib/api";
import { buildOverviewSummary } from "../lib/overview";

export function Overview({
  apiKey,
  data,
  onLogs,
  onWorker,
  onWorkers,
}: {
  apiKey: string;
  data: RuntimeData;
  onLogs(): void;
  onWorker(name: string, version: string): void;
  onWorkers(): void;
}) {
  const seriesQuery = useQuery({
    queryKey: ["cpanel", "overview", "series"],
    queryFn: () =>
      apiJson<{
        partialWindow?: boolean;
        points: Array<{
          durationP95Ms: number | null;
          errorCount: number;
          requestCount: number;
        }>;
      }>(
        apiKey,
        "/api/admin/observability/series?windowMs=300000&bucketMs=15000",
      ),
    refetchInterval: 5000,
  });
  const eventsQuery = useQuery({
    queryKey: ["cpanel", "overview", "events"],
    queryFn: () =>
      apiJson<{ events: OperationalEvent[] }>(
        apiKey,
        "/api/admin/observability/events?limit=5",
      ),
    refetchInterval: 5000,
  });
  const summary = buildOverviewSummary(data, seriesQuery.data?.points ?? []);
  const pool = data.metricsStats?.pool ?? {};
  const events = eventsQuery.data?.events ?? [];
  const partial =
    seriesQuery.isError ||
    eventsQuery.isError ||
    !data.metricsStats ||
    seriesQuery.data?.partialWindow;
  return (
    <div className="grid gap-4">
      {partial && <Badge variant="secondary">Partial window</Badge>}

      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <MetricCard
          description={`${summary.routable} routable versions`}
          icon={BoxIcon}
          label="Apps"
          value={String(summary.apps)}
        />
        <MetricCard
          description="Loaded by this runtime"
          icon={RouteIcon}
          label="Worker versions"
          value={String(summary.versions)}
        />
        <MetricCard
          description={`${summary.errors5m} errors · ${summary.p95Ms == null ? "no latency data" : `${summary.p95Ms} ms p95`}`}
          icon={ActivityIcon}
          label="Requests · 5 min"
          value={String(summary.requests5m)}
        />
        <MetricCard
          description={`${summary.processes.queued} queued · ${summary.processes.terminating} terminating`}
          icon={CpuIcon}
          label="Processes"
          value={`${summary.processes.active} active · ${summary.processes.idle} idle`}
        />
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            Needs attention
            {summary.attention.length > 0 && (
              <Badge variant="secondary">{summary.attention.length}</Badge>
            )}
          </CardTitle>
          <CardDescription>
            Routing, passive health, recent errors and capacity pressure.
          </CardDescription>
          <CardAction>
            <Button onClick={onWorkers} size="sm" variant="outline">
              Review workers
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent>
          {summary.attention.length === 0 ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <CircleCheckIcon className="size-4 text-emerald-600" />
              No disabled, degraded or failing versions and no recent capacity
              signals.
            </div>
          ) : (
            <div className="grid gap-2 md:grid-cols-2 xl:grid-cols-3">
              {summary.attention.slice(0, 6).map((item, index) => (
                <button
                  className="flex items-start gap-3 rounded-lg border p-3 text-left transition-colors hover:bg-muted/50"
                  key={`${item.kind}-${item.name}-${item.version ?? index}`}
                  onClick={() => {
                    if (item.kind === "recent-error") onLogs();
                    else if (item.version) onWorker(item.name, item.version);
                    else onWorkers();
                  }}
                  type="button"
                >
                  <CircleAlertIcon
                    className={`mt-0.5 size-4 shrink-0 ${item.severity === "critical" ? "text-rose-600" : "text-amber-600"}`}
                  />
                  <span className="min-w-0 flex-1">
                    <strong className="block truncate text-sm">
                      {item.name}
                      {item.version ? `@${item.version}` : ""}
                    </strong>
                    <small className="text-muted-foreground">
                      {item.detail}
                    </small>
                  </span>
                  <ChevronRightIcon className="my-auto size-4 shrink-0 text-muted-foreground" />
                </button>
              ))}
            </div>
          )}
        </CardContent>
      </Card>

      <div className="grid gap-4 lg:grid-cols-4">
        <Card className="lg:col-span-2">
          <CardHeader>
            <CardTitle>Runtime capacity</CardTitle>
            <CardDescription>
              Current process and queue snapshot from this instance.
            </CardDescription>
            <CardAction>
              <Badge variant="outline">Live · /metrics/stats</Badge>
            </CardAction>
          </CardHeader>
          <CardContent className="grid grid-cols-2 gap-5 sm:grid-cols-3">
            {[
              ["Active", summary.processes.active],
              ["Idle", summary.processes.idle],
              ["Queued", summary.processes.queued],
              ["Max processes", summary.processes.max],
              [
                "Cache hit rate",
                summary.cacheHitRate == null
                  ? "No data"
                  : `${summary.cacheHitRate}%`,
              ],
              ["Spawn p50", `${pool.spawnLatencyMsP50 ?? 0} ms`],
            ].map(([label, value]) => (
              <Stat key={String(label)} label={String(label)} value={value} />
            ))}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Health distribution</CardTitle>
            <CardDescription>Passive five-minute window.</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-3 text-sm">
            {[
              ["Healthy", summary.health.healthy, "bg-emerald-500"],
              ["Degraded", summary.health.degraded, "bg-amber-500"],
              ["Failing", summary.health.failing, "bg-rose-500"],
              [
                "Unobserved",
                summary.health.unobserved,
                "bg-muted-foreground/40",
              ],
            ].map(([label, value, color]) => (
              <div className="flex items-center gap-2" key={String(label)}>
                <span className={`size-2 rounded-full ${color}`} />
                <span className="flex-1 text-muted-foreground">{label}</span>
                <strong>{value}</strong>
              </div>
            ))}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Access context</CardTitle>
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

      <div className="grid gap-4 xl:grid-cols-3">
        <Card className="xl:col-span-2">
          <CardHeader>
            <CardTitle>Workers at a glance</CardTitle>
            <CardDescription>
              Highest request volume since the current runtime snapshot reset.
            </CardDescription>
            <CardAction>
              <Button onClick={onWorkers} size="sm" variant="outline">
                All workers
              </Button>
            </CardAction>
          </CardHeader>
          <CardContent>
            <div className="overflow-hidden rounded-lg border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Worker</TableHead>
                    <TableHead>Health</TableHead>
                    <TableHead className="text-right">Requests</TableHead>
                    <TableHead className="text-right">P95</TableHead>
                    <TableHead className="text-right">Queue</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {summary.topWorkers.map((worker) => (
                    <TableRow
                      className="cursor-pointer"
                      key={`${worker.name}@${worker.version}`}
                      onClick={() => onWorker(worker.name, worker.version)}
                    >
                      <TableCell>
                        <span className="font-mono text-xs">
                          {worker.name}@{worker.version}
                        </span>
                      </TableCell>
                      <TableCell>
                        <HealthIndicator worker={worker} />
                      </TableCell>
                      <TableCell className="text-right">
                        {worker.requestTotal ?? 0}
                      </TableCell>
                      <TableCell className="text-right">
                        {worker.requestDurationMsP95 ?? 0} ms
                      </TableCell>
                      <TableCell className="text-right">
                        {worker.queued ?? 0}
                      </TableCell>
                    </TableRow>
                  ))}
                  {summary.topWorkers.length === 0 && (
                    <TableRow>
                      <TableCell
                        className="h-24 text-center text-muted-foreground"
                        colSpan={5}
                      >
                        No runtime worker metrics yet.
                      </TableCell>
                    </TableRow>
                  )}
                </TableBody>
              </Table>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Recent activity</CardTitle>
            <CardDescription>
              Latest bounded operational events retained locally.
            </CardDescription>
            <CardAction>
              <Button onClick={onLogs} size="sm" variant="outline">
                View logs
              </Button>
            </CardAction>
          </CardHeader>
          <CardContent className="grid gap-1">
            {events.map((event, index) => (
              <button
                className="flex items-start gap-3 rounded-md px-2 py-2 text-left hover:bg-muted/50"
                key={String(event.id ?? `${event.atMs}-${index}`)}
                onClick={onLogs}
                type="button"
              >
                <ListIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
                <span className="min-w-0 flex-1">
                  <strong className="block truncate text-sm">
                    {event.kind ?? event.source ?? "runtime event"}
                  </strong>
                  <small className="block truncate text-muted-foreground">
                    {event.worker
                      ? `${event.worker}@${event.version ?? "latest"}`
                      : "runtime"}
                    {event.outcome ? ` · ${event.outcome}` : ""}
                  </small>
                </span>
                <time className="shrink-0 text-xs text-muted-foreground">
                  {event.atMs ? formatAge(event.atMs) : "recent"}
                </time>
              </button>
            ))}
            {!eventsQuery.isLoading && events.length === 0 && (
              <p className="py-8 text-center text-sm text-muted-foreground">
                No recent operational events.
              </p>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
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

function Stat({ label, value }: { label: string; value: unknown }) {
  return (
    <div>
      <span className="text-xs font-medium uppercase text-muted-foreground">
        {label}
      </span>
      <strong className="mt-1 block text-xl">{String(value ?? 0)}</strong>
    </div>
  );
}

function HealthIndicator({ worker }: { worker: RuntimeWorker }) {
  const status = worker.health?.status ?? "unobserved";
  const color =
    status === "healthy"
      ? "bg-emerald-500"
      : status === "failing"
        ? "bg-rose-500"
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

function formatAge(timestamp: number) {
  if (!timestamp) return "recently";
  const seconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
  if (seconds < 5) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  return `${Math.floor(minutes / 60)}h ago`;
}
