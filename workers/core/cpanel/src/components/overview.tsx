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
import { type AttentionItem, buildOverviewSummary } from "../lib/overview";
import { type TranslationKey, useI18n } from "../lib/i18n";

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
  const { locale, t } = useI18n();
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
      {partial && (
        <Badge variant="secondary">{t("overview.partialWindow")}</Badge>
      )}

      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <MetricCard
          description={message(
            t,
            summary.routable === 1
              ? "overview.routableVersion"
              : "overview.routableVersions",
            { count: summary.routable },
          )}
          icon={BoxIcon}
          label={t("overview.apps")}
          value={String(summary.apps)}
        />
        <MetricCard
          description={t("overview.loadedByRuntime")}
          icon={RouteIcon}
          label={t("overview.workerVersions")}
          value={String(summary.versions)}
        />
        <MetricCard
          description={message(
            t,
            summary.errors5m === 1
              ? "overview.errorAndLatency"
              : "overview.errorsAndLatency",
            {
              count: summary.errors5m,
              latency:
                summary.p95Ms == null
                  ? t("overview.noLatencyData")
                  : `${summary.p95Ms} ms p95`,
            },
          )}
          icon={ActivityIcon}
          label={t("overview.requestsFiveMinutes")}
          value={String(summary.requests5m)}
        />
        <MetricCard
          description={message(t, "overview.queuedAndTerminating", {
            queued: summary.processes.queued,
            terminating: summary.processes.terminating,
          })}
          icon={CpuIcon}
          label={t("overview.processes")}
          value={message(t, "overview.activeAndIdle", {
            active: summary.processes.active,
            idle: summary.processes.idle,
          })}
        />
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            {t("overview.needsAttention")}
            {summary.attention.length > 0 && (
              <Badge variant="secondary">{summary.attention.length}</Badge>
            )}
          </CardTitle>
          <CardDescription>
            {t("overview.attentionDescription")}
          </CardDescription>
          <CardAction>
            <Button onClick={onWorkers} size="sm" variant="outline">
              {t("overview.attentionReview")}
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent>
          {summary.attention.length === 0 ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <CircleCheckIcon className="size-4 text-emerald-600" />
              {t("overview.attentionEmpty")}
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
                      {attentionDetail(item, t)}
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
            <CardTitle>{t("overview.runtimeCapacity")}</CardTitle>
            <CardDescription>
              {t("overview.currentSnapshot")}
            </CardDescription>
            <CardAction>
              <Badge variant="outline">{t("overview.liveMetrics")}</Badge>
            </CardAction>
          </CardHeader>
          <CardContent className="grid grid-cols-2 gap-5 sm:grid-cols-3">
            {[
              [t("overview.active"), summary.processes.active],
              [t("overview.idle"), summary.processes.idle],
              [t("overview.queued"), summary.processes.queued],
              [t("overview.maxProcesses"), summary.processes.max],
              [
                t("overview.cacheHitRate"),
                summary.cacheHitRate == null
                  ? t("overview.noData")
                  : `${summary.cacheHitRate}%`,
              ],
              [t("overview.spawnP50"), `${pool.spawnLatencyMsP50 ?? 0} ms`],
            ].map(([label, value]) => (
              <Stat key={String(label)} label={String(label)} value={value} />
            ))}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>{t("overview.healthDistribution")}</CardTitle>
            <CardDescription>{t("overview.passiveWindow")}</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-3 text-sm">
            {[
              [t("overview.healthy"), summary.health.healthy, "bg-emerald-500"],
              [t("overview.degraded"), summary.health.degraded, "bg-amber-500"],
              [t("overview.failing"), summary.health.failing, "bg-rose-500"],
              [
                t("overview.unobserved"),
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
            <CardTitle>{t("overview.accessContext")}</CardTitle>
          </CardHeader>
          <CardContent className="grid gap-3 text-sm">
            {[
              [t("overview.principal"), data.principal.name],
              [t("overview.role"), data.principal.role],
              [t("overview.namespaces"), data.principal.namespaces?.join(", ")],
              [t("overview.controlPlane"), t("overview.rootKeyGate")],
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
            <CardTitle>{t("overview.workersAtGlance")}</CardTitle>
            <CardDescription>
              {t("overview.workerDescription")}
            </CardDescription>
            <CardAction>
              <Button onClick={onWorkers} size="sm" variant="outline">
                {t("overview.allWorkers")}
              </Button>
            </CardAction>
          </CardHeader>
          <CardContent>
            <div className="overflow-hidden rounded-lg border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("overview.tableWorker")}</TableHead>
                    <TableHead>{t("overview.tableHealth")}</TableHead>
                    <TableHead className="text-right">
                      {t("overview.tableRequests")}
                    </TableHead>
                    <TableHead className="text-right">
                      {t("overview.tableP95")}
                    </TableHead>
                    <TableHead className="text-right">
                      {t("overview.tableQueue")}
                    </TableHead>
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
                          <span
                            className="block max-w-[18rem] truncate"
                            title={`${worker.name}@${worker.version}`}
                          >
                            {worker.name}@{worker.version}
                          </span>
                        </span>
                      </TableCell>
                      <TableCell>
                        <HealthIndicator worker={worker} t={t} />
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
                        {t("overview.noWorkerMetrics")}
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
            <CardTitle>{t("overview.recentActivity")}</CardTitle>
            <CardDescription>
              {t("overview.eventsDescription")}
            </CardDescription>
            <CardAction>
              <Button onClick={onLogs} size="sm" variant="outline">
                {t("overview.viewLogs")}
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
                    {event.kind ?? event.source ?? t("overview.runtimeEvent")}
                  </strong>
                  <small className="block truncate text-muted-foreground">
                    {event.worker
                      ? `${event.worker}@${event.version ?? t("overview.latestVersion")}`
                      : t("overview.runtime")}
                    {event.outcome ? ` · ${event.outcome}` : ""}
                  </small>
                </span>
                <time className="shrink-0 text-xs text-muted-foreground">
                  {event.atMs
                    ? formatAge(event.atMs, locale)
                    : t("overview.recent")}
                </time>
              </button>
            ))}
            {!eventsQuery.isLoading && events.length === 0 && (
              <p className="py-8 text-center text-sm text-muted-foreground">
                {t("overview.eventsEmpty")}
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

function HealthIndicator({
  worker,
  t,
}: {
  worker: RuntimeWorker;
  t(key: TranslationKey): string;
}) {
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
      {status === "healthy"
        ? t("overview.healthy")
        : status === "degraded"
          ? t("overview.degraded")
          : status === "failing"
            ? t("overview.failing")
            : status === "unobserved"
              ? t("overview.unobserved")
              : status}
    </span>
  );
}

function attentionDetail(
  item: AttentionItem,
  t: (key: TranslationKey) => string,
) {
  switch (item.detail.type) {
    case "disabled":
      return t("overview.attention.disabled");
    case "health":
      return t(
        item.detail.status === "failing"
          ? "overview.attention.failing"
          : "overview.attention.degraded",
      );
    case "capacity":
      return message(t, "overview.attention.capacity", {
        queued: item.detail.queued,
        rejected: item.detail.rejected,
        timedOut: item.detail.timedOut,
      });
    case "recent-error":
      return `${message(
        t,
        item.detail.count === 1
          ? "overview.attention.oneError"
          : "overview.attention.manyErrors",
        { count: item.detail.count },
      )}${item.detail.code ? ` · ${item.detail.code}` : ""}`;
  }
}

function message(
  t: (key: TranslationKey) => string,
  key: TranslationKey,
  values: Record<string, number | string>,
) {
  return Object.entries(values).reduce(
    (text, [name, value]) => text.replaceAll(`{${name}}`, String(value)),
    t(key),
  );
}

function formatAge(timestamp: number, locale: string) {
  const seconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
  const formatter = new Intl.RelativeTimeFormat(locale, { numeric: "auto" });
  if (seconds < 5) return formatter.format(0, "second");
  if (seconds < 60) return formatter.format(-seconds, "second");
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return formatter.format(-minutes, "minute");
  return formatter.format(-Math.floor(minutes / 60), "hour");
}
