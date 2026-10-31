import { ensureAppDom } from "./app.dom";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

// Root-app test: importing ./main mounts the REAL app (router, query client,
// providers) into the #root element app.dom.ts creates; these tests drive
// that mounted instance end to end. One continuous story per file: the app
// instance and its query client persist across the tests below, so the
// tests are ordered by design (login → change → Shell → 403).
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

// The single stateful surface of the fake runtime: every endpoint routes by
// the x-api-key header (the session credential) and the scripted answers
// (login, password change, the 403 flip) are swapped per test. Lenient
// defaults answer every incidental read (login-options, metrics, series,
// events) so the whole app runs against this one mock.
const app = {
  loginByUser: {} as Record<string, { body: unknown; status?: number }>,
  sessionByToken: {} as Record<string, SessionBody>,
  workersByToken: {} as Record<string, unknown[]>,
  passwordAnswer: null as null | { body: unknown; status?: number },
  workers403: false,
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
      if (path === "/api/admin/workers") {
        if (app.workers403)
          return jsonResponse(
            {
              code: "PASSWORD_CHANGE_REQUIRED",
              message: "password change required",
            },
            403,
          );
        return jsonResponse({ workers: app.workersByToken[token ?? ""] ?? [] });
      }
      if (path === "/metrics/stats") return jsonResponse({ pool: {}, workers: [] });
      if (path.startsWith("/api/admin/observability/series"))
        return jsonResponse({ partialWindow: false, points: [] });
      if (path.startsWith("/api/admin/observability/events"))
        return jsonResponse({ events: [] });
      return jsonResponse({});
    },
  );
}

// Initial state: no stored session; the pending user logs in with a
// temporary password. The app mounts (into #root) with exactly this state.
localStorage.clear();
sessionStorage.clear();
localStorage.setItem("edger.cpanel.locale", "pt-BR");
app.loginByUser["temp-op"] = {
  body: { mustChangePassword: true, token: "ses-temp-1" },
};
app.sessionByToken["ses-temp-1"] = session(true);
app.sessionByToken["ses-temp-2"] = session(false, {
  isRoot: true,
  name: "op",
  role: "root",
});
app.workersByToken["ses-temp-2"] = [
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

describe("root app: a temporary password gates the Shell", () => {
  beforeAll(() => {
    // bun evaluates all test files' modules before any test runs; re-assert
    // the shared DOM globals in case a sibling afterAll dropped them.
    ensureAppDom();
  });
  afterAll(() => {
    globalThis.fetch = originalFetch;
  });

  it("shows the change screen for a login answering mustChangePassword, without calling loadAll", async () => {
    await screen.findByText("Acesso administrativo");
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("Usuário"), "temp-op");
    await user.type(screen.getByLabelText("Senha"), "temp0!Senha123");
    await user.click(screen.getByRole("button", { name: "Entrar" }));

    await screen.findByText("Defina uma nova senha");
    // The Shell must not render for a pending session.
    expect(screen.queryByText("Visão geral")).toBeNull();
    // The session is stored: the change screen works off it.
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-temp-1");
    // loadAll is not called: no data route ran at all.
    expect(
      app.calls.filter((call) => call.url.includes("/api/admin/workers")),
    ).toEqual([]);
    // The identity read carried the new token.
    const sessionCall = app.calls.find(
      (call) => call.url.endsWith("/api/admin/session"),
    );
    expect(sessionCall?.token).toBe("ses-temp-1");
    // The form is present: current, new, confirm and the shared policy hint.
    expect(screen.getByLabelText("Senha atual")).toBeTruthy();
    expect(screen.getByLabelText("Nova senha")).toBeTruthy();
    expect(screen.getByLabelText("Confirmar nova senha")).toBeTruthy();
    expect(
      screen.getByText(
        "12 a 128 caracteres, com pelo menos uma letra, um dígito e um símbolo.",
      ),
    ).toBeTruthy();
  }, 30000);

  it("logout from the change screen returns to login and clears the session", async () => {
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Sair" }));

    await screen.findByText("Acesso administrativo");
    expect(screen.queryByText("Defina uma nova senha")).toBeNull();
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    // The best-effort revocation ran with the session token.
    expect(app.logoutTokens).toContain("ses-temp-1");
  }, 15000);

  it("logging in again as the pending user returns to the change screen", async () => {
    const user = userEvent.setup();
    // The title renders immediately; the password form only appears once
    // /login-options answered — wait for the form, not the card.
    await user.type(await screen.findByLabelText("Usuário"), "temp-op");
    await user.type(screen.getByLabelText("Senha"), "temp0!Senha123");
    await user.click(screen.getByRole("button", { name: "Entrar" }));

    await screen.findByText("Defina uma nova senha");
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-temp-1");
  }, 30000);

  it("a successful change stores the returned token and loads the Shell", async () => {
    app.passwordAnswer = { body: { token: "ses-temp-2" } };
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Senha atual"), "temp0!Senha123");
    await user.type(screen.getByLabelText("Nova senha"), "n0va!Senha456");
    await user.type(screen.getByLabelText("Confirmar nova senha"), "n0va!Senha456");
    await user.click(screen.getByRole("button", { name: "Alterar senha" }));

    // The Shell loads normally after the change.
    await screen.findByRole("heading", { name: "Visão geral" });
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-temp-2");
    // The change went out with the old session credential and both fields.
    const changeCall = app.calls.find((call) =>
      call.url.endsWith("/api/admin/me/password"),
    );
    expect(changeCall?.token).toBe("ses-temp-1");
    expect(changeCall?.method).toBe("POST");
    // loadAll now ran under the new token.
    expect(
      app.calls.some(
        (call) =>
          call.url.endsWith("/api/admin/workers") && call.token === "ses-temp-2",
      ),
    ).toBe(true);
  }, 30000);

  it("a 403 PASSWORD_CHANGE_REQUIRED from a data route never clears the stored session", async () => {
    // The operator marks the password temporary again: the data routes 403
    // and /session answers the flag. The Shell refetches on the manual
    // refresh (the real UI path) and the app must end on the change screen
    // with the session intact.
    app.workers403 = true;
    app.sessionByToken["ses-temp-2"] = session(true, {
      isRoot: true,
      name: "op",
      role: "root",
    });
    const workersCallsBefore = app.calls.filter(
      (call) =>
        call.url.endsWith("/api/admin/workers") && call.token === "ses-temp-2",
    ).length;

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Atualizar" }));

    // The error path only lands after the failed retry: wait until the
    // 403 data route was hit at least twice, so the error state (and the
    // effect that could clear the session) is guaranteed to have run.
    await waitFor(
      () => {
        const after = app.calls.filter(
          (call) =>
            call.url.endsWith("/api/admin/workers") &&
            call.token === "ses-temp-2",
        ).length;
        expect(after - workersCallsBefore).toBeGreaterThanOrEqual(2);
      },
      { timeout: 30000 },
    );
    // The credential is still valid: the change screen comes back and the
    // stored session SURVIVED the 403.
    await screen.findByText("Defina uma nova senha", undefined, { timeout: 15000 });
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-temp-2");
    expect(screen.queryByRole("heading", { name: "Visão geral" })).toBeNull();
  }, 45000);
});
