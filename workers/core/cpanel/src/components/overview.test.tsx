import "./routing-policy.dom";
import { afterAll, afterEach, describe, expect, it, vi } from "vitest";

import type { RuntimeData } from "../lib/api";

if (!Element.prototype.getAnimations) {
  Element.prototype.getAnimations = () => [];
}

const { cleanup, render, screen, waitFor } = await import(
  "@testing-library/react"
);
const userEvent = (await import("@testing-library/user-event")).default;
const { QueryClient, QueryClientProvider } = await import(
  "@tanstack/react-query"
);
const { I18nProvider, useI18n } = await import("../lib/i18n");
const { Overview } = await import("./overview");

const originalFetch = globalThis.fetch;
const fetchMock = vi.fn();
globalThis.fetch = fetchMock;

function jsonResponse(body: unknown) {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
  });
}

const runtimeData: RuntimeData = {
  metricsStats: {
    pool: { cacheHits: 8, cacheMisses: 2, spawnLatencyMsP50: 14 },
    workers: [
      {
        name: "checkout",
        requestDurationMsP95: 18,
        requestTotal: 12,
        version: "7.1.0",
      },
    ],
  },
  principal: { name: "operator-1", namespaces: ["store"], role: "admin" },
  workerErrors: { checkout: { count: 2, latest: { code: "UPSTREAM_503" } } },
  workers: [
    {
      kind: "FetchHandler",
      name: "checkout",
      namespace: "store",
      status: "enabled",
      version: "7.1.0",
    },
  ],
};

function LocaleControl() {
  const { setLocale } = useI18n();
  return (
    <button onClick={() => setLocale("en-US")} type="button">
      Switch to English
    </button>
  );
}

function renderOverview(data: RuntimeData = runtimeData, onWorker = vi.fn()) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <I18nProvider>
        <LocaleControl />
        <Overview
          apiKey="test-key"
          data={data}
          onLogs={vi.fn()}
          onWorker={onWorker}
          onWorkers={vi.fn()}
        />
      </I18nProvider>
    </QueryClientProvider>,
  );
}

describe("Overview localization", () => {
  afterEach(() => {
    cleanup();
    fetchMock.mockReset();
    localStorage.clear();
  });

  afterAll(() => {
    globalThis.fetch = originalFetch;
  });

  it("renders Portuguese data and switches the same overview to English", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/api/admin/observability/series")) {
        return jsonResponse({
          points: [{ durationP95Ms: 18, errorCount: 1, requestCount: 12 }],
        });
      }
      if (url.includes("/api/admin/observability/events")) {
        return jsonResponse({
          events: [
            {
              atMs: Date.now() - 90_000,
              id: "event-1",
              kind: "worker.deploy",
              outcome: "completed",
              worker: "checkout",
              version: "7.1.0",
            },
          ],
        });
      }
      throw new Error(`Unexpected overview request: ${url}`);
    });

    renderOverview();

    expect(await screen.findByText("Requer atenção")).toBeTruthy();
    expect(screen.getByText("Capacidade do runtime")).toBeTruthy();
    expect(screen.getByText("Distribuição de saúde")).toBeTruthy();
    expect(screen.getByText("Contexto de acesso")).toBeTruthy();
    expect(screen.getByText("Workers em resumo")).toBeTruthy();
    expect(screen.getByText("Atividade recente")).toBeTruthy();
    expect(screen.getByText("Requisições · 5 min")).toBeTruthy();
    expect(screen.getByText("2 erros recentes · UPSTREAM_503")).toBeTruthy();
    expect(screen.getAllByText("Não observado").length).toBeGreaterThan(0);
    expect(await screen.findByText(/há \d+ minutos?/)).toBeTruthy();
    expect(document.body.textContent).not.toMatch(/\bago\b|just now|\brecent\b/i);
    expect(screen.getByText("operator-1")).toBeTruthy();
    expect(screen.getByText("checkout@7.1.0")).toBeTruthy();
    expect(screen.getByText("worker.deploy")).toBeTruthy();
    expect(screen.getByText(/completed/)).toBeTruthy();

    await userEvent.setup().click(
      screen.getByRole("button", { name: "Switch to English" }),
    );
    expect(await screen.findByText("Needs attention")).toBeTruthy();
    expect(screen.getByText("Runtime capacity")).toBeTruthy();
    expect(screen.getAllByText("Unobserved").length).toBeGreaterThan(0);
    expect(screen.getByText("2 recent errors · UPSTREAM_503")).toBeTruthy();
    await waitFor(() => expect(screen.getByText(/ago/)).toBeTruthy());
  });

  it("keeps a long worker identity intact while constraining its display", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/api/admin/observability/series")) {
        return jsonResponse({ points: [] });
      }
      if (url.includes("/api/admin/observability/events")) {
        return jsonResponse({ events: [] });
      }
      throw new Error(`Unexpected overview request: ${url}`);
    });
    const longName = `worker-${"long-".repeat(20)}`;
    const longVersion = "v2026.09.28-build-1234567890";
    const identity = `${longName}@${longVersion}`;
    const workerData: RuntimeData = {
      ...runtimeData,
      metricsStats: {
        ...(runtimeData.metricsStats ?? {}),
        workers: [
          {
            name: longName,
            requestTotal: 1,
            version: longVersion,
          },
        ],
      },
      workers: [
        {
          ...runtimeData.workers[0],
          name: longName,
          version: longVersion,
        },
      ],
    };
    const onWorker = vi.fn();
    renderOverview(workerData, onWorker);

    const identityElement = await screen.findByText(identity);
    expect(identityElement.textContent).toBe(identity);
    expect(identityElement.classList.contains("truncate")).toBe(true);
    expect(identityElement.className).toContain("max-w-[18rem]");
    expect(identityElement.getAttribute("title")).toBe(identity);

    await userEvent.setup().click(identityElement.closest("tr") as HTMLElement);
    expect(onWorker).toHaveBeenCalledWith(longName, longVersion);
  });
});
