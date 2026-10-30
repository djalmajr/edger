import { ensureAppDom } from "./app.dom";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

// Root-app test: importing ./main mounts the REAL app (router, query client,
// providers) into the #root element app.dom.ts creates; these tests drive
// that mounted instance end to end. Story: sign in with credential A, see
// A's data, log out, sign in with credential B — none of A's data may
// render for B. The app instance and its query client persist across the
// tests below, so the tests are ordered by design.
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
  loginByUser: {} as Record<string, { body: unknown; status?: number }>,
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

function session(principal: Record<string, unknown>): SessionBody {
  return {
    mustChangePassword: false,
    principal,
    tenantRoutingEnabled: false,
    weightedRoutingEnabled: false,
  };
}

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
      if (path === "/api/admin/login") {
        const body = JSON.parse(String(init?.body)) as { username: string };
        const answer = app.loginByUser[body.username] ?? {
          status: 401,
          body: { message: "invalid credentials" },
        };
        return jsonResponse(answer.body, answer.status ?? 200);
      }
      if (path === "/api/admin/logout") {
        app.logoutTokens.push(token ?? "");
        return new Response(null, { status: 204 });
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
      if (path === "/metrics/stats") return jsonResponse({ pool: {}, workers: [] });
      if (path.startsWith("/api/admin/observability/series"))
        return jsonResponse({ partialWindow: false, points: [] });
      if (path.startsWith("/api/admin/observability/events"))
        return jsonResponse({ events: [] });
      return jsonResponse({});
    },
  );
}

// Initial state: no stored session; two non-pending operators exist. The
// app mounts (into #root) with exactly this state.
localStorage.clear();
sessionStorage.clear();
localStorage.setItem("edger.cpanel.locale", "pt-BR");
app.loginByUser["ops-a"] = {
  body: { mustChangePassword: false, token: "ses-cred-a" },
};
app.loginByUser["ops-b"] = {
  body: { mustChangePassword: false, token: "ses-cred-b" },
};
app.sessionByToken["ses-cred-a"] = session({
  isRoot: true,
  name: "ops-a",
  role: "root",
});
app.sessionByToken["ses-cred-b"] = session({
  isRoot: true,
  name: "ops-b",
  role: "root",
});
app.workersByToken["ses-cred-a"] = [
  {
    kind: "fetch",
    name: "alpha-worker",
    origin: "user",
    source: "user",
    status: "enabled",
    version: "1.0.0",
    visibility: "internal",
  },
];
app.workersByToken["ses-cred-b"] = [
  {
    kind: "fetch",
    name: "beta-worker",
    origin: "user",
    source: "user",
    status: "enabled",
    version: "1.0.0",
    visibility: "internal",
  },
];
installFetch();
await import("./main");

describe("root app: no credential renders another credential's cached data", () => {
  beforeAll(() => {
    ensureAppDom();
  });
  afterAll(() => {
    globalThis.fetch = originalFetch;
  });

  // Snapshot of A's data reads taken BEFORE the credential switch: the
  // switch must add B's reads and none more of A's.
  let aWorkersAtSwitch = 0;

  it("shows credential A's data for credential A", async () => {
    const user = userEvent.setup();
    await screen.findByText("Acesso administrativo");
    // The title renders immediately; the password form only appears once
    // /login-options answered — wait for the form, not the card.
    await user.type(await screen.findByLabelText("Usuário"), "ops-a");
    await user.type(screen.getByLabelText("Senha"), "ops-a!Passw0rd1");
    await user.click(screen.getByRole("button", { name: "Entrar" }));

    await screen.findByRole("heading", { name: "Visão geral" });
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-cred-a");
    await user.click(screen.getByRole("button", { name: "Workers" }));
    await screen.findByText("alpha-worker");
    aWorkersAtSwitch = app.calls.filter(
      (call) =>
        call.url.endsWith("/api/admin/workers") && call.token === "ses-cred-a",
    ).length;
    expect(aWorkersAtSwitch).toBeGreaterThan(0);
  }, 30000);

  it("after logout and login with another credential, none of A's data renders", async () => {
    const user = userEvent.setup();
    // Log out through the account menu (the real UI path).
    await user.click(screen.getByRole("button", { name: "Conta" }));
    await user.click(await screen.findByRole("menuitem", { name: "Sair" }));
    await screen.findByText("Acesso administrativo");
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();

    // Sign in with credential B.
    await user.type(await screen.findByLabelText("Usuário"), "ops-b");
    await user.type(screen.getByLabelText("Senha"), "ops-b!Passw0rd1");
    await user.click(screen.getByRole("button", { name: "Entrar" }));
    // The Shell restores the last URL (the workers view A visited): B opens
    // directly there, which is exactly where its data must come from the
    // network under B's token.
    await screen.findByRole("heading", { name: "Workers" });
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-cred-b");

    // B sees its own data…
    await screen.findByText("beta-worker");
    // …and none of A's is rendered for B (the cache of A's credential was
    // removed on logout/credential change — nothing stale could surface).
    expect(screen.queryByText("alpha-worker")).toBeNull();
    // …and every data read of B's session went out with B's token, while
    // A's data reads stayed exactly at the pre-switch count (no stale read
    // under A's token after the switch).
    await waitFor(() => {
      const bWorkersCalls = app.calls.filter(
        (call) =>
          call.url.endsWith("/api/admin/workers") && call.token === "ses-cred-b",
      ).length;
      expect(bWorkersCalls).toBeGreaterThan(0);
      expect(
        app.calls.filter(
          (call) =>
            call.url.endsWith("/api/admin/workers") &&
            call.token === "ses-cred-a",
        ).length,
      ).toBe(aWorkersAtSwitch);
    });
  }, 30000);
});
