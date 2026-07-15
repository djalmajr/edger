export type Principal = { name?: string; namespaces?: string[]; role?: string };
export type Worker = {
  healthCheck?: {
    method?: string;
    mode?: string;
    path?: string;
    timeoutMs?: number;
  } | null;
  kind: unknown;
  name: string;
  namespace?: string | null;
  origin?: string;
  source?: string;
  status: string;
  version: string;
};
export type RuntimeWorker = {
  activeProcesses?: number;
  health?: {
    failureCount?: number;
    observedAtMs?: number | null;
    sampleCount?: number;
    status?: string;
    successCount?: number;
    windowMs?: number;
  };
  idleProcesses?: number;
  maxProcesses?: number;
  name: string;
  requestDurationMsP95?: number;
  requestTotal?: number;
  state?: string;
  terminatingProcesses?: number;
  version: string;
};
export type RuntimeData = {
  metricsStats: {
    pool?: Record<string, number>;
    workers?: RuntimeWorker[];
  } | null;
  principal: Principal;
  workerErrors: Record<string, { count?: number; latest?: { code?: string } }>;
  workers: Worker[];
};

export async function apiJson<T>(
  apiKey: string,
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("x-api-key", apiKey);
  const response = await fetch(path, { ...init, headers });
  const text = await response.text();
  const data = text ? (JSON.parse(text) as unknown) : {};
  if (!response.ok) {
    const message =
      typeof data === "object" &&
      data !== null &&
      "message" in data &&
      typeof data.message === "string"
        ? data.message
        : `${response.status} ${response.statusText}`;
    throw new Error(message);
  }
  return data as T;
}

export async function loadAll(apiKey: string): Promise<RuntimeData> {
  const session = await apiJson<{ principal: Principal }>(
    apiKey,
    "/api/admin/session",
  );
  const [workers, workerErrors, metricsStats] = await Promise.all([
    apiJson<{ workers: Worker[] }>(apiKey, "/api/admin/workers").then(
      (data) => data.workers ?? [],
    ),
    apiJson<{ summary: RuntimeData["workerErrors"] }>(
      apiKey,
      "/api/admin/workers/error-summary",
    )
      .then((data) => data.summary ?? {})
      .catch(() => ({})),
    apiJson<NonNullable<RuntimeData["metricsStats"]>>(
      apiKey,
      "/metrics/stats",
    ).catch(() => null),
  ]);
  return { metricsStats, principal: session.principal, workerErrors, workers };
}

export function kindLabel(kind: unknown) {
  if (kind == null) return "-";
  if (typeof kind === "string") return kind;
  if (typeof kind === "object") return Object.keys(kind)[0] ?? "-";
  return String(kind);
}

export function workerUrl(worker: Worker, latest = false) {
  const scoped = worker.namespace
    ? `@${worker.namespace}/${worker.name}`
    : worker.name;
  return latest ? `/${scoped}` : `/${scoped}@${worker.version}`;
}

export function compareSemver(a: string, b: string) {
  const left = a.split(".").map((part) => Number.parseInt(part, 10) || 0);
  const right = b.split(".").map((part) => Number.parseInt(part, 10) || 0);
  for (let index = 0; index < 3; index += 1)
    if ((left[index] ?? 0) !== (right[index] ?? 0))
      return (left[index] ?? 0) - (right[index] ?? 0);
  return 0;
}
