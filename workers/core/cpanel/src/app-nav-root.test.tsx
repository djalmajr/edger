import { ensureAppDom } from "./app.dom";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

// Root-app test: importing ./main mounts the REAL app (router, query client,
// providers) into the #root element app.dom.ts creates; these tests drive
// that mounted instance end to end. One continuous story per file: a
// session restored from sessionStorage (the reload entry path) for a ROOT
// principal — the Shell must offer the full navigation, including Users,
// Active sessions and Settings. The app instance and its query client
// persist across the tests below, so the tests are ordered by design.
const { configure, screen } = await import("@testing-library/react");

configure({ asyncUtilTimeout: 10000 });
const { SESSION_KEY } = await import("./lib/api");

const originalFetch = globalThis.fetch;
const fetchMock = vi.fn();
globalThis.fetch = fetchMock;

type SessionBody = {
  mustChangePassword: boolean;
  principal: Record<string, unknown>;
  tenantRoutingEnabled: boolean;
  weightedRoutingEnabled: boolean;
};

const app = {
  sessionByToken: {} as Record<string, SessionBody>,
  workersByToken: {} as Record<string, unknown[]>,
};

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
    status,
  });
}

function installFetch() {
  fetchMock.mockImplementation(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const path = new URL(url).pathname;
      const token = init ? new Headers(init.headers).get("x-api-key") : null;
      if (path === "/api/admin/login-options")
        return jsonResponse({ passwordEnabled: true, rootSeeded: true });
      if (path === "/api/admin/logout")
        return new Response(null, { status: 204 });
      if (path === "/api/admin/session") {
        const answer = app.sessionByToken[token ?? ""];
        if (!answer) return jsonResponse({ message: "unauthorized" }, 401);
        return jsonResponse(answer);
      }
      if (path === "/api/admin/workers/error-summary")
        return jsonResponse({ summary: {} });
      if (path === "/api/admin/workers")
        return jsonResponse({ workers: app.workersByToken[token ?? ""] ?? [] });
      if (path === "/metrics/stats") return jsonResponse({ pool: {}, workers: [] });
      if (path.startsWith("/api/admin/observability/series"))
        return jsonResponse({ partialWindow: false, points: [] });
      if (path.startsWith("/api/admin/observability/events"))
        return jsonResponse({ events: [] });
      return jsonResponse({});
    },
  );
}

// Initial state: a non-pending session restored from sessionStorage (the
// reload entry path) for a ROOT principal. The app mounts (into #root) with
// exactly this state.
localStorage.clear();
sessionStorage.clear();
localStorage.setItem("edger.cpanel.locale", "en-US");
sessionStorage.setItem(SESSION_KEY, "ses-restored");
app.sessionByToken["ses-restored"] = {
  mustChangePassword: false,
  principal: {
    isRoot: true,
    name: "root",
    role: "root",
    keyPrefix: "ses",
    permissions: ["*"],
  },
  tenantRoutingEnabled: false,
  weightedRoutingEnabled: false,
};
app.workersByToken["ses-restored"] = [
  {
    kind: "fetch",
    name: "hello",
    origin: "user",
    source: "user",
    status: "enabled",
    version: "1.0.0",
    visibility: "internal",
  },
];
installFetch();
await import("./main");

describe("review: Shell navigation matrix — root", () => {
  beforeAll(() => ensureAppDom());
  afterAll(() => {
    globalThis.fetch = originalFetch;
  });

  it("renders the navigation for root", async () => {
    await screen.findByRole("heading", { name: "Overview" });
    // Root is a user manager AND the only Settings audience: all three
    // entries are visible.
    expect(screen.queryByRole("button", { name: "Users" })).not.toBeNull();
    expect(screen.queryByRole("button", { name: "Active sessions" })).not.toBeNull();
    expect(screen.queryByRole("button", { name: "Settings" })).not.toBeNull();
  }, 30000);
});
