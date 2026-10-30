import { ensureAppDom } from "./app.dom";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

// Root-app test: importing ./main mounts the REAL app (router, query client,
// providers) into the #root element app.dom.ts creates; these tests drive
// that mounted instance end to end. One continuous story per file: a
// session restored from sessionStorage with a temporary password (the
// reload entry path), then the pasted-token entry path. The app instance
// and its query client persist across the tests below, so the tests are
// ordered by design.
const { configure, screen, within } = await import("@testing-library/react");
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
  passwordAnswer: null as null | { body: unknown; status?: number },
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
        const answer =
          app.passwordAnswer ?? {
            status: 401,
            body: { message: "invalid credentials" },
          };
        return jsonResponse(answer.body, answer.status ?? 200);
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

describe("root app: a restored pending session stays on the change screen", () => {
  beforeAll(() => {
    ensureAppDom();
  });
  afterAll(() => {
    globalThis.fetch = originalFetch;
  });

  it("shows the change screen for the restored session without clearing it or calling loadAll", async () => {
    await screen.findByText("Defina uma nova senha");
    // The stored session is NOT cleared: the flag, not a missing session,
    // is what the screen shows.
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-restored");
    // loadAll is not called for a pending session.
    expect(
      app.calls.filter((call) => call.url.includes("/api/admin/workers")),
    ).toEqual([]);
    // The Shell must not render.
    expect(screen.queryByText("Visão geral")).toBeNull();
    // The identity read carried the restored token.
    const sessionCall = app.calls.find(
      (call) => call.url.endsWith("/api/admin/session"),
    );
    expect(sessionCall?.token).toBe("ses-restored");
  }, 30000);

  it("does not dismiss the change screen on Escape and has no close control", async () => {
    // There is no close button on the card at all.
    expect(screen.queryByRole("button", { name: /fechar|close/i })).toBeNull();
    const user = userEvent.setup();
    await user.keyboard("{Escape}");
    // Escape does nothing: the screen is still up and the session intact.
    expect(screen.getByText("Defina uma nova senha")).toBeTruthy();
    expect(screen.queryByText("Acesso administrativo")).toBeNull();
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-restored");
  }, 15000);

  it("logout from the change screen returns to login and clears the session", async () => {
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Sair" }));

    await screen.findByText("Acesso administrativo");
    expect(screen.queryByText("Defina uma nova senha")).toBeNull();
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    expect(app.logoutTokens).toContain("ses-restored");
  }, 15000);

  it("a pasted pending token shows the change screen and keeps the stored session", async () => {
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Entrar com token" }));
    const tokenForm = screen.getByLabelText("Token").closest("form");
    if (!tokenForm) throw new Error("token form not found");
    await user.type(screen.getByLabelText("Token"), "ses-restored");
    await user.click(within(tokenForm).getByRole("button", { name: "Entrar" }));

    await screen.findByText("Defina uma nova senha");
    // The pasted token is stored as the session, not rejected.
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-restored");
    // Still no data route for a pending credential.
    expect(
      app.calls.filter((call) => call.url.includes("/api/admin/workers")),
    ).toEqual([]);
  }, 30000);

  it("shows the 400 server message and keeps the user on the change screen", async () => {
    // New password equal to the current one: the server answers 400
    // VALIDATION_ERROR with its fixed copy.
    app.passwordAnswer = {
      status: 400,
      body: {
        code: "VALIDATION_ERROR",
        message: "new password must differ from the current one",
      },
    };
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Senha atual"), "temp0!Senha123");
    await user.type(screen.getByLabelText("Nova senha"), "temp0!Senha123");
    await user.type(
      screen.getByLabelText("Confirmar nova senha"),
      "temp0!Senha123",
    );
    await user.click(screen.getByRole("button", { name: "Alterar senha" }));

    // The server's message is shown verbatim and the user stays on the
    // screen: the session is intact and the Shell did not render.
    await screen.findByText("new password must differ from the current one");
    expect(screen.getByText("Defina uma nova senha")).toBeTruthy();
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-restored");
    expect(screen.queryByText("Visão geral")).toBeNull();
    expect(app.calls.find((call) => call.url.endsWith("/api/admin/workers"))).toBeUndefined();
  }, 30000);

  it("a successful change stores the new token and shows the Shell", async () => {
    app.passwordAnswer = { body: { token: "ses-restored-2" } };
    const user = userEvent.setup();
    const nextInput = screen.getByLabelText("Nova senha");
    const confirmInput = screen.getByLabelText("Confirmar nova senha");
    // The rejected attempt kept the form values: overwrite both.
    await user.clear(nextInput);
    await user.type(nextInput, "n0va!Senha456");
    await user.clear(confirmInput);
    await user.type(confirmInput, "n0va!Senha456");
    await user.click(screen.getByRole("button", { name: "Alterar senha" }));

    await screen.findByRole("heading", { name: "Visão geral" });
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-restored-2");
    const changeCall = app.calls.find((call) =>
      call.url.endsWith("/api/admin/me/password"),
    );
    expect(changeCall?.token).toBe("ses-restored");
    expect(
      app.calls.some(
        (call) =>
          call.url.endsWith("/api/admin/workers") &&
          call.token === "ses-restored-2",
      ),
    ).toBe(true);
  }, 30000);
});
