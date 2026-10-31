import { ensureAppDom } from "./app.dom";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

// Root-app test: importing ./main mounts the REAL app (router, query client,
// providers) into the #root element app.dom.ts creates; this test drives
// that mounted instance end to end. Story: a session restored from
// sessionStorage (non-pending), then the Files view of a worker — deleting a
// file must update the rendered listing (the delete-success cache write has
// to hit the same key the listing reads).
const { configure, screen, within, waitFor } = await import(
  "@testing-library/react"
);
const { default: userEvent } = await import("@testing-library/user-event");
const { SESSION_KEY } = await import("./lib/api");

configure({ asyncUtilTimeout: 10000 });

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
  calls: [] as Array<{ method: string; token: string | null; url: string }>,
};

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
    status,
  });
}

function session(
  principal: Record<string, unknown> = { name: "op" },
): SessionBody {
  return {
    mustChangePassword: false,
    principal,
    tenantRoutingEnabled: false,
    weightedRoutingEnabled: false,
  };
}

// The server state of the worker's file list: the delete flips it, and the
// listing route reflects it (a refetch would answer the updated list — the
// test proves the cache write/update alone is enough).
let deleted = false;
function installFetch() {
  fetchMock.mockImplementation(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const path = new URL(url).pathname;
      const method = init?.method ?? "GET";
      const token = init ? new Headers(init.headers).get("x-api-key") : null;
      app.calls.push({ method, token, url });
      if (path === "/api/admin/workers/hello/files/delete") {
        deleted = true;
        // The route answers with the ROOT listing of the version.
        return jsonResponse({
          deleted: ["old.txt"],
          entries: [],
          errors: [],
          revision: "2",
        });
      }
      if (path === "/api/admin/workers/hello/files")
        return jsonResponse({
          entries: deleted
            ? []
            : [{ kind: "file", name: "old.txt", size: 10 }],
        });
      if (path === "/api/admin/login-options")
        return jsonResponse({ passwordEnabled: true, rootSeeded: true });
      if (path === "/api/admin/login")
        return jsonResponse({ message: "invalid credentials" }, 401);
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
      if (path === "/metrics/stats")
        return jsonResponse({ pool: {}, workers: [] });
      if (path.startsWith("/api/admin/observability/series"))
        return jsonResponse({ partialWindow: false, points: [] });
      if (path.startsWith("/api/admin/observability/events"))
        return jsonResponse({ events: [] });
      return jsonResponse({});
    },
  );
}

// Initial state: a non-pending session restored from sessionStorage. The
// app mounts (into #root) with exactly this state.
localStorage.clear();
sessionStorage.clear();
localStorage.setItem("edger.cpanel.locale", "pt-BR");
sessionStorage.setItem(SESSION_KEY, "ses-restored");
app.sessionByToken["ses-restored"] = session({
  isRoot: true,
  name: "op",
  role: "root",
});
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

describe("root app: deleting a file updates the rendered listing", () => {
  beforeAll(() => {
    ensureAppDom();
  });
  afterAll(() => {
    globalThis.fetch = originalFetch;
  });

  it("removes a deleted file from the current listing after the server success", async () => {
    const user = userEvent.setup();
    await screen.findByRole("heading", { name: "Visão geral" });
    await user.click(screen.getByRole("button", { name: "Workers" }));
    await user.click(
      await screen.findByRole("button", { name: /hello.*Fetch/i }),
    );
    await user.click(screen.getByRole("button", { name: "Browse files" }));
    await screen.findByText("old.txt");
    await user.click(screen.getByRole("button", { name: "Delete old.txt" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Delete" }));
    // The server accepted the delete…
    await waitFor(() => expect(deleted).toBe(true));
    // …the confirmation closed…
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    // …and the listing reflects it: the deleted file is gone.
    await waitFor(() => expect(screen.queryByText("old.txt")).toBeNull(), {
      timeout: 1000,
    });
  }, 30000);
});
