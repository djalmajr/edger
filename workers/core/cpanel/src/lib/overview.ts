import type { RuntimeData, RuntimeWorker } from "./api";

export type OverviewSeriesPoint = {
  durationP95Ms: number | null;
  errorCount: number;
  requestCount: number;
};

export type AttentionItem = {
  detail: string;
  kind: "capacity" | "disabled" | "health" | "recent-error";
  name: string;
  severity: "critical" | "warning";
  version?: string;
};

export type OverviewSummary = ReturnType<typeof buildOverviewSummary>;

export function buildOverviewSummary(
  data: RuntimeData,
  points: OverviewSeriesPoint[],
) {
  const runtimeWorkers = data.metricsStats?.workers ?? [];
  const health = {
    degraded: 0,
    failing: 0,
    healthy: 0,
    unobserved: 0,
  };
  const processes = {
    active: 0,
    idle: 0,
    max: 0,
    queued: 0,
    terminating: 0,
  };
  const attention: AttentionItem[] = [];

  for (const worker of data.workers) {
    if (worker.status === "disabled") {
      attention.push({
        detail: "Version is not routable",
        kind: "disabled",
        name: worker.name,
        severity: "warning",
        version: worker.version,
      });
    }
  }

  for (const worker of runtimeWorkers) {
    const status = normalizeHealth(worker.health?.status);
    health[status] += 1;
    processes.active += worker.activeProcesses ?? 0;
    processes.idle += worker.idleProcesses ?? 0;
    processes.max += worker.maxProcesses ?? 0;
    processes.queued += worker.queued ?? 0;
    processes.terminating += worker.terminatingProcesses ?? 0;

    if (status === "degraded" || status === "failing") {
      attention.push({
        detail:
          status === "failing"
            ? "Passive health is failing"
            : "Passive health is degraded",
        kind: "health",
        name: worker.name,
        severity: status === "failing" ? "critical" : "warning",
        version: worker.version,
      });
    }

    const rejected = worker.rejectedTotal ?? 0;
    const timedOut = worker.timeoutTotal ?? 0;
    const queued = worker.queued ?? 0;
    if (rejected > 0 || timedOut > 0 || queued > 0) {
      attention.push({
        detail: `${queued} queued · ${rejected} rejected · ${timedOut} timed out`,
        kind: "capacity",
        name: worker.name,
        severity: "warning",
        version: worker.version,
      });
    }
  }

  for (const [name, value] of Object.entries(data.workerErrors)) {
    if ((value.count ?? 0) === 0) continue;
    attention.push({
      detail: `${value.count} recent error${value.count === 1 ? "" : "s"}${value.latest?.code ? ` · ${value.latest.code}` : ""}`,
      kind: "recent-error",
      name,
      severity: "warning",
    });
  }

  const requests5m = points.reduce(
    (sum, point) => sum + point.requestCount,
    0,
  );
  const errors5m = points.reduce((sum, point) => sum + point.errorCount, 0);
  const p95Ms = [...points]
    .reverse()
    .find((point) => point.durationP95Ms != null)?.durationP95Ms;
  const pool = data.metricsStats?.pool;
  const hits = pool?.cacheHits ?? 0;
  const misses = pool?.cacheMisses ?? 0;
  const cacheHitRate =
    hits + misses === 0 ? null : Math.round((hits / (hits + misses)) * 100);
  const runtimeWorkerKeys = new Set(
    runtimeWorkers.map((worker) => `${worker.name}@${worker.version}`),
  );
  const catalogOnlyWorkers: RuntimeWorker[] = data.workers
    .filter(
      (worker) =>
        worker.status !== "disabled" &&
        !runtimeWorkerKeys.has(`${worker.name}@${worker.version}`),
    )
    .map((worker) => ({
      health: { status: "unobserved" },
      name: worker.name,
      version: worker.version,
    }));
  const topWorkers = [...runtimeWorkers, ...catalogOnlyWorkers]
    .sort(compareWorkerActivity)
    .slice(0, 6);
  const appKeys = new Set(
    data.workers.map((worker) => `${worker.namespace ?? ""}/${worker.name}`),
  );
  const status: "critical" | "degraded" | "healthy" =
    health.failing > 0
      ? "critical"
      : attention.length > 0 || errors5m > 0
        ? "degraded"
        : "healthy";

  return {
    apps: appKeys.size,
    attention,
    cacheHitRate,
    errors5m,
    health,
    p95Ms: p95Ms ?? null,
    processes,
    requests5m,
    routable: data.workers.filter((worker) => worker.status !== "disabled")
      .length,
    status,
    topWorkers,
    versions: data.workers.length,
  };
}

function normalizeHealth(
  status?: string,
): "degraded" | "failing" | "healthy" | "unobserved" {
  if (["degraded", "failing", "healthy"].includes(status ?? "")) {
    return status as "degraded" | "failing" | "healthy";
  }
  return "unobserved";
}

function compareWorkerActivity(a: RuntimeWorker, b: RuntimeWorker) {
  const byRequests = (b.requestTotal ?? 0) - (a.requestTotal ?? 0);
  if (byRequests !== 0) return byRequests;
  return (b.requestDurationMsP95 ?? 0) - (a.requestDurationMsP95 ?? 0);
}
