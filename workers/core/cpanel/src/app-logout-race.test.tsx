import { ensureAppDom } from "./app.dom";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

// Root-app test: importing ./main mounts the REAL app (router, query client,
// providers) into the #root element app.dom.ts creates; this test drives
// that mounted instance end to end. Story: a pending session restored from
// sessionStorage, the forced change screen, then Logout clicked while the
// change request is still in flight. When the late success arrives, the
// logout must stay final: the session storage stays empty, the login screen
// stays, and no request carries the rotated token.
const { configure, screen, waitFor } = await import("@testing-library/react");
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
  logoutTokens: [] as string[],
  calls: [] as Array<{ method: string; token: string | null; url: string }>,
};

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
    status,
  });
}

function session(
  mustChangePassword: boolean,
  principal: Record<string, unknown> = { name: "op" },
): SessionBody {
  return {
    mustChangePassword,
    principal,
    tenantRoutingEnabled: false,
    weightedRoutingEnabled: false,
  };
}

// The password change never answers on its own: `releasePassword` resolves
// it with the rotated-token success, and `passwordBodyRead` flips when the
// app actually reads that body — a deterministic point at which any (buggy)
// restore would have run.
let releasePassword: (() => void) | undefined;
let passwordBodyRead = false;
function installFetch() {
  fetchMock.mockImplementation(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const path = new URL(url).pathname;
      const method = init?.method ?? "GET";
      const token = init ? new Headers(init.headers).get("x-api-key") : null;
      app.calls.push({ method, token, url });
      if (path === "/api/admin/login-options")
        return jsonResponse({ passwordEnabled: true, rootSeeded: true });
      if (path === "/api/admin/login")
        return jsonResponse({ message: "invalid credentials" }, 401);
      if (path === "/api/admin/logout") {
        app.logoutTokens.push(token ?? "");
        return new Response(null, { status: 204 });
      }
      if (path === "/api/admin/me/password") {
        return new Promise<Response>((resolve) => {
          releasePassword = () => {
            const response = jsonResponse({ token: "ses-restored-2" });
            const readJson = response.json.bind(response);
            response.json = () => {
              passwordBodyRead = true;
              return readJson();
            };
            resolve(response);
          };
        });
      }
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

// Initial state: a pending session restored from sessionStorage (the reload
// entry path). The app mounts (into #root) with exactly this state.
localStorage.clear();
sessionStorage.clear();
localStorage.setItem("edger.cpanel.locale", "pt-BR");
sessionStorage.setItem(SESSION_KEY, "ses-restored");
app.sessionByToken["ses-restored"] = session(true);
app.sessionByToken["ses-restored-2"] = session(false, {
  isRoot: true,
  name: "op",
  role: "root",
});
app.workersByToken["ses-restored-2"] = [
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

describe("root app: a change finishing after logout stays ignored", () => {
  beforeAll(() => {
    ensureAppDom();
  });
  afterAll(() => {
    globalThis.fetch = originalFetch;
  });

  it("keeps the logout final: storage empty, login shown, no rotated-token read", async () => {
    await screen.findByText("Defina uma nova senha");
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Senha atual"), "temp0!Senha123");
    await user.type(screen.getByLabelText("Nova senha"), "n0va!Senha456");
    await user.type(screen.getByLabelText("Confirmar nova senha"), "n0va!Senha456");
    await user.click(screen.getByRole("button", { name: "Alterar senha" }));
    // The request is in flight now (holding its promise) — click Logout.
    await waitFor(() => expect(releasePassword).toBeTypeOf("function"));
    await user.click(screen.getByRole("button", { name: "Sair" }));
    await screen.findByText("Acesso administrativo");
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    expect(app.logoutTokens).toContain("ses-restored");
    // Release the late success.
    releasePassword!();
    // Wait until the app has actually read the body: by then any restore
    // would have run (its write is synchronous after the body read).
    await waitFor(() => expect(passwordBodyRead).toBe(true));
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    expect(screen.queryByText("Acesso administrativo")).not.toBeNull();
    expect(screen.queryByText("Defina uma nova senha")).toBeNull();
    // The completion started no query: no request carries the rotated
    // token.
    expect(
      app.calls.some((call) => call.token === "ses-restored-2"),
    ).toBe(false);
  }, 30000);
});
