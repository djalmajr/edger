import { describe, expect, it } from "vitest";

import type { RuntimeData, RuntimeWorker, Worker } from "./api";
import { buildOverviewSummary } from "./overview";

function makeRuntimeWorker(
  overrides: Partial<RuntimeWorker> = {},
): RuntimeWorker {
  return {
    activeProcesses: 0,
    health: { status: "healthy" },
    idleProcesses: 1,
    maxProcesses: 2,
    name: "orders",
    queued: 0,
    rejectedTotal: 0,
    requestDurationMsP95: 12,
    requestTotal: 10,
    terminatingProcesses: 0,
    timeoutTotal: 0,
    version: "1.0.0",
    ...overrides,
  };
}

function makeWorker(overrides: Partial<Worker> = {}): Worker {
  return {
    kind: "FetchHandler",
    name: "orders",
    status: "enabled",
    version: "1.0.0",
    ...overrides,
  };
}

function makeRuntimeData(overrides: Partial<RuntimeData> = {}): RuntimeData {
  return {
    metricsStats: {
      pool: { cacheHits: 8, cacheMisses: 2 },
      workers: [makeRuntimeWorker()],
    },
    principal: { name: "operator", namespaces: ["*"], role: "admin" },
    workerErrors: {},
    workers: [makeWorker()],
    ...overrides,
  };
}

describe("buildOverviewSummary", () => {
  it("marks the runtime critical when a worker is failing", () => {
    const data = makeRuntimeData({
      metricsStats: {
        pool: { cacheHits: 8, cacheMisses: 2 },
        workers: [
          makeRuntimeWorker({
            activeProcesses: 2,
            health: { status: "failing" },
            idleProcesses: 1,
            maxProcesses: 4,
            queued: 3,
            terminatingProcesses: 1,
          }),
        ],
      },
    });

    const summary = buildOverviewSummary(data, [
      { durationP95Ms: 34, errorCount: 2, requestCount: 9 },
      { durationP95Ms: 21, errorCount: 0, requestCount: 4 },
    ]);

    expect(summary.status).toBe("critical");
    expect(summary.requests5m).toBe(13);
    expect(summary.errors5m).toBe(2);
    expect(summary.p95Ms).toBe(21);
    expect(summary.health.failing).toBe(1);
    expect(summary.processes).toEqual({
      active: 2,
      idle: 1,
      max: 4,
      queued: 3,
      terminating: 1,
    });
    expect(summary.cacheHitRate).toBe(80);
  });

  it("marks attention signals as degraded without treating unobserved as unhealthy", () => {
    const data = makeRuntimeData({
      metricsStats: {
        pool: {},
        workers: [
          makeRuntimeWorker({
            health: { status: "unobserved" },
            rejectedTotal: 2,
          }),
        ],
      },
      workers: [makeWorker({ status: "disabled" })],
    });

    const summary = buildOverviewSummary(data, []);

    expect(summary.status).toBe("degraded");
    expect(summary.health.unobserved).toBe(1);
    expect(summary.attention.map((item) => item.kind)).toEqual([
      "disabled",
      "capacity",
    ]);
  });

  it("ranks workers by request volume and then latency", () => {
    const data = makeRuntimeData({
      metricsStats: {
        pool: {},
        workers: [
          makeRuntimeWorker({ name: "slow", requestDurationMsP95: 90 }),
          makeRuntimeWorker({ name: "busy", requestTotal: 50 }),
          makeRuntimeWorker({ name: "fast", requestDurationMsP95: 5 }),
          makeRuntimeWorker({ name: "fourth", requestTotal: 4 }),
          makeRuntimeWorker({ name: "fifth", requestTotal: 3 }),
        ],
      },
      workers: [
        makeWorker({ name: "busy" }),
        makeWorker({ name: "catalog-only" }),
      ],
    });

    const summary = buildOverviewSummary(data, []);

    expect(summary.topWorkers.map((worker) => worker.name)).toEqual([
      "busy",
      "slow",
      "fast",
      "fourth",
      "fifth",
      "catalog-only",
    ]);
    expect(summary.topWorkers.at(-1)?.health?.status).toBe("unobserved");
  });
});
