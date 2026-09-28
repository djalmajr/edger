import { ensureLoginDom } from "./login.dom";
import { afterAll, beforeAll, afterEach, describe, expect, it, vi } from "vitest";

const { cleanup, render, screen, waitFor, within } = await import(
  "@testing-library/react"
);
const { default: userEvent } = await import("@testing-library/user-event");
const { I18nProvider } = await import("../lib/i18n");
const { ThemeProvider } = await import("@edger/ui/lib/theme");
const { SESSION_KEY } = await import("../lib/api");
const { AdminLogin } = await import("./login");

const originalFetch = globalThis.fetch;
const fetchMock = vi.fn();
globalThis.fetch = fetchMock;

function jsonResponse(body: unknown, init?: ResponseInit) {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
    ...init,
  });
}

// loadAll after a successful credential: session first, then the three
// parallel reads fired in workers / error-summary / metrics order.
function queueLoadAllSuccess() {
  fetchMock
    .mockResolvedValueOnce(jsonResponse({ principal: { isRoot: true } }))
    .mockResolvedValueOnce(jsonResponse({ workers: [] }))
    .mockResolvedValueOnce(jsonResponse({ summary: {} }))
    .mockResolvedValueOnce(jsonResponse({ pool: {} }));
}

function renderLogin(onAuthenticated: (token: string) => void) {
  render(
    <I18nProvider>
      <ThemeProvider>
        <AdminLogin onAuthenticated={onAuthenticated} />
      </ThemeProvider>
    </I18nProvider>,
  );
}

function storeOnAuthenticated() {
  return vi.fn((token: string) => {
    sessionStorage.setItem(SESSION_KEY, token);
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe("AdminLogin", () => {
  beforeAll(() => {
    // bun evaluates all test files' modules before any test runs; a sibling
    // file's afterAll may have dropped the shared DOM globals in between.
    ensureLoginDom();
  });
  afterEach(() => {
    cleanup();
    fetchMock.mockReset();
    localStorage.clear();
    sessionStorage.clear();
  });

  it("logs in with user/password, validates the session token, and stores it", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    const onAuthenticated = storeOnAuthenticated();
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse({ passwordEnabled: true, rootSeeded: true }),
      )
      .mockResolvedValueOnce(jsonResponse({ token: "ses-abc" }));
    queueLoadAllSuccess();
    renderLogin(onAuthenticated);

    const title = await screen.findByText("Acesso administrativo");
    expect(title).toBeTruthy();
    // Card structure: soft centered card, wrapping-safe header, compact
    // controls that never squeeze the title block off screen.
    expect(title.closest("[data-slot='card']")?.className).toContain(
      "w-full max-w-md",
    );
    expect(title.closest("[data-slot='card-header'] .min-w-0")).toBeTruthy();
    expect(screen.getByText("Root é seedado no primeiro boot.")).toBeTruthy();
    expect(screen.getByLabelText("Usuário")).toBeTruthy();
    expect(screen.getByLabelText("Senha")).toBeTruthy();

    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Usuário"), "root");
    await user.type(screen.getByLabelText("Senha"), "s3cr3t!");
    await user.click(screen.getByRole("button", { name: "Entrar" }));

    await waitFor(() =>
      expect(onAuthenticated).toHaveBeenCalledWith("ses-abc"),
    );
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-abc");

    const loginCall = fetchMock.mock.calls.find(
      ([url]) => String(url).endsWith("/api/admin/login"),
    );
    expect(loginCall).toBeTruthy();
    const [, loginInit] = loginCall as unknown as [string, RequestInit];
    expect(loginInit.method).toBe("POST");
    expect(new Headers(loginInit.headers).get("x-api-key")).toBeNull();
    expect(JSON.parse(String(loginInit.body))).toEqual({
      username: "root",
      password: "s3cr3t!",
    });
    const optionsCall = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(String(optionsCall[0])).not.toContain("/api/admin/login?");
    expect(optionsCall[0]).toMatch(/api\/admin\/login-options$/);
    // The validated session token rides the header on the loadAll reads.
    const sessionCalls = fetchMock.mock.calls.filter(
      ([, init]) =>
        (init as RequestInit | undefined) &&
        new Headers((init as RequestInit).headers).get("x-api-key") ===
          "ses-abc",
    );
    expect(sessionCalls.length).toBeGreaterThanOrEqual(1);
  });

  it("shows progress only on the token form and restores both buttons after failure", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    const onAuthenticated = storeOnAuthenticated();
    const pendingSession = deferred<Response>();
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse({ passwordEnabled: true, rootSeeded: true }),
      )
      .mockReturnValueOnce(pendingSession.promise);
    renderLogin(onAuthenticated);

    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "Entrar com token" }),
    );
    await user.type(screen.getByLabelText("Usuário"), "root");
    await user.type(screen.getByLabelText("Senha"), "fake-password");
    await user.type(screen.getByLabelText("Token"), "fake-token");

    const passwordForm = screen.getByLabelText("Senha").closest("form");
    const tokenForm = screen.getByLabelText("Token").closest("form");
    if (!passwordForm || !tokenForm) throw new Error("login form not found");
    await user.click(within(tokenForm).getByRole("button", { name: "Entrar" }));

    expect(
      within(tokenForm)
        .getByRole("button", { name: "Entrando…" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(
      within(passwordForm)
        .getByRole("button", { name: "Entrar" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(
      within(passwordForm).queryByRole("button", { name: "Entrando…" }),
    ).toBeNull();

    pendingSession.resolve(
      jsonResponse({ message: "unauthorized" }, { status: 401 }),
    );
    await screen.findByText("Token inválido.");
    expect(
      within(tokenForm)
        .getByRole("button", { name: "Entrar" })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(
      within(passwordForm)
        .getByRole("button", { name: "Entrar" })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(onAuthenticated).not.toHaveBeenCalled();
  });

  it("shows progress only on the password form and restores both buttons after failure", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    const onAuthenticated = storeOnAuthenticated();
    const pendingLogin = deferred<Response>();
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse({ passwordEnabled: true, rootSeeded: true }),
      )
      .mockReturnValueOnce(pendingLogin.promise);
    renderLogin(onAuthenticated);

    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "Entrar com token" }),
    );
    await user.type(screen.getByLabelText("Usuário"), "root");
    await user.type(screen.getByLabelText("Senha"), "fake-password");
    await user.type(screen.getByLabelText("Token"), "fake-token");

    const passwordForm = screen.getByLabelText("Senha").closest("form");
    const tokenForm = screen.getByLabelText("Token").closest("form");
    if (!passwordForm || !tokenForm) throw new Error("login form not found");
    await user.click(
      within(passwordForm).getByRole("button", { name: "Entrar" }),
    );

    expect(
      within(passwordForm)
        .getByRole("button", { name: "Entrando…" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(
      within(tokenForm)
        .getByRole("button", { name: "Entrar" })
        .hasAttribute("disabled"),
    ).toBe(true);
    expect(
      within(tokenForm).queryByRole("button", { name: "Entrando…" }),
    ).toBeNull();

    pendingLogin.resolve(
      jsonResponse({ message: "invalid credentials" }, { status: 401 }),
    );
    await screen.findByText("Usuário ou senha inválidos.");
    expect(
      within(passwordForm)
        .getByRole("button", { name: "Entrar" })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(
      within(tokenForm)
        .getByRole("button", { name: "Entrar" })
        .hasAttribute("disabled"),
    ).toBe(false);
    expect(onAuthenticated).not.toHaveBeenCalled();
  });

  it("treats 401 as invalid credentials and never shows the password", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    const onAuthenticated = storeOnAuthenticated();
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse({ passwordEnabled: true, rootSeeded: true }),
      )
      .mockResolvedValueOnce(
        jsonResponse({ message: "invalid credentials" }, { status: 401 }),
      );
    renderLogin(onAuthenticated);
    const user = userEvent.setup();
    await user.type(
      await screen.findByLabelText("Usuário"),
      "root",
    );
    await user.type(screen.getByLabelText("Senha"), "s3cr3t!");
    await user.click(screen.getByRole("button", { name: "Entrar" }));

    await screen.findByText("Usuário ou senha inválidos.");
    expect(onAuthenticated).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    expect(document.body.textContent).not.toContain("s3cr3t!");
  });

  it("distinguishes 429, 503, and network errors", async () => {
    const cases: Array<{
      body: unknown;
      init?: ResponseInit;
      reject?: Error;
      message: string;
    }> = [
      {
        body: { message: "login_rate_limited" },
        init: { status: 429 },
        message: "Muitas tentativas. Tente novamente em alguns instantes.",
      },
      {
        body: { message: "maintenance" },
        init: { status: 503 },
        message: "O login está indisponível no momento.",
      },
      {
        body: undefined,
        reject: new TypeError("fetch failed"),
        message:
          "Não foi possível alcançar o runtime. Verifique sua conexão.",
      },
    ];
    for (const testCase of cases) {
      cleanup();
      fetchMock.mockReset();
      localStorage.setItem("edger.cpanel.locale", "pt-BR");
      const onAuthenticated = storeOnAuthenticated();
      fetchMock.mockResolvedValueOnce(
        jsonResponse({ passwordEnabled: true, rootSeeded: true }),
      );
      // Reject lazily: an eagerly rejected Promise is reported as unhandled
      // before the awaited fetch consumes it.
      if (testCase.reject) {
        fetchMock.mockImplementationOnce(async () => {
          throw testCase.reject as Error;
        });
      } else {
        fetchMock.mockResolvedValueOnce(
          jsonResponse(testCase.body, testCase.init),
        );
      }
      renderLogin(onAuthenticated);
      const user = userEvent.setup();
      await user.type(await screen.findByLabelText("Usuário"), "root");
      await user.type(screen.getByLabelText("Senha"), "s3cr3t!");
      await user.click(screen.getByRole("button", { name: "Entrar" }));
      await screen.findByText(testCase.message);
      expect(onAuthenticated).not.toHaveBeenCalled();
    }
  });

  it("hides the password form when the runtime says passwords are off, and keeps the token path working", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    const onAuthenticated = storeOnAuthenticated();
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ passwordEnabled: false, rootSeeded: false }),
    );
    renderLogin(onAuthenticated);
    await screen.findByText(
      "O login por senha ainda não está disponível. Entre com token.",
    );
    expect(screen.queryByLabelText("Usuário")).toBeNull();
    expect(screen.queryByLabelText("Senha")).toBeNull();

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Entrar com token" }));
    queueLoadAllSuccess();
    await user.type(screen.getByLabelText("Token"), "root-key");
    await user.click(screen.getByRole("button", { name: "Entrar" }));
    await waitFor(() =>
      expect(onAuthenticated).toHaveBeenCalledWith("root-key"),
    );
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("root-key");
  });

  it("uses the three honest copy states for login-options", async () => {
    const states: Array<{
      body: unknown;
      line: string;
      formVisible: boolean;
    }> = [
      {
        body: { passwordEnabled: false, rootSeeded: false },
        line: "O login por senha ainda não está disponível. Entre com token.",
        formVisible: false,
      },
      {
        // Unknown/malformed: no assertion about password state, and no seed.
        body: { bogus: true },
        line: "Você também pode entrar com token.",
        formVisible: true,
      },
      {
        // Passwords on but seed not confirmed: neutral sentence only.
        body: { passwordEnabled: true, rootSeeded: false },
        line: "Você também pode entrar com token.",
        formVisible: true,
      },
    ];
    for (const state of states) {
      cleanup();
      fetchMock.mockReset();
      localStorage.setItem("edger.cpanel.locale", "pt-BR");
      fetchMock.mockResolvedValueOnce(jsonResponse(state.body));
      renderLogin(storeOnAuthenticated());
      await screen.findByText(state.line);
      expect(screen.queryByText("Root é seedado no primeiro boot.")).toBeNull();
      if (state.formVisible) {
        expect(screen.queryByLabelText("Senha")).toBeTruthy();
      } else {
        expect(screen.queryByLabelText("Senha")).toBeNull();
      }
    }
  });

  it("keeps the screenshot copy when login-options confirms the seeded root", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ passwordEnabled: true, rootSeeded: true }),
    );
    renderLogin(storeOnAuthenticated());
    await screen.findByText("Root é seedado no primeiro boot.");
    expect(
      screen.queryByText("Você também pode entrar com token."),
    ).toBeNull();
    expect(screen.queryByText("O login por senha ainda não está disponível. Entre com token.")).toBeNull();
  });

  it("reports a rejected token without validating or storing it", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    const onAuthenticated = storeOnAuthenticated();
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse({ passwordEnabled: true, rootSeeded: true }),
      )
      .mockResolvedValueOnce(
        jsonResponse({ message: "unauthorized" }, { status: 401 }),
      );
    renderLogin(onAuthenticated);
    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "Entrar com token" }),
    );
    queueLoadAllSuccess();
    const tokenForm = screen.getByLabelText("Token").closest("form");
    if (!tokenForm) throw new Error("token form not found");
    await user.type(screen.getByLabelText("Token"), "wrong-key");
    await user.click(within(tokenForm).getByRole("button", { name: "Entrar" }));
    await screen.findByText("Token inválido.");
    expect(onAuthenticated).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    expect(document.body.textContent).not.toContain("wrong-key");
  });

  it("renders the translated en-US screen", async () => {
    localStorage.setItem("edger.cpanel.locale", "en-US");
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse({ passwordEnabled: true, rootSeeded: true }),
      )
      .mockResolvedValueOnce(
        jsonResponse({ message: "invalid credentials" }, { status: 401 }),
      );
    renderLogin(storeOnAuthenticated());
    await screen.findByText("Admin access");
    await screen.findByText("Root is seeded on first boot.");
    expect(screen.getByText("Sign in with token")).toBeTruthy();
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Username"), "root");
    await user.type(screen.getByLabelText("Password"), "s3cr3t!");
    await user.click(screen.getByRole("button", { name: "Sign in" }));
    await screen.findByText("Invalid username or password.");
  });

  it("exposes accessible visibility toggles for password and token", async () => {
    localStorage.setItem("edger.cpanel.locale", "pt-BR");
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ passwordEnabled: true, rootSeeded: true }),
    );
    renderLogin(storeOnAuthenticated());
    const user = userEvent.setup();
    await screen.findByLabelText("Usuário");

    const passwordInput = screen.getByLabelText("Senha") as HTMLInputElement;
    expect(passwordInput.type).toBe("password");
    const passwordToggle = screen.getByRole("button", {
      name: "Mostrar senha",
    });
    await user.click(passwordToggle);
    expect(passwordInput.type).toBe("text");
    expect(
      screen.getByRole("button", { name: "Ocultar senha" }),
    ).toBeTruthy();

    await user.click(screen.getByRole("button", { name: "Entrar com token" }));
    const tokenInput = screen.getByLabelText("Token") as HTMLInputElement;
    expect(tokenInput.type).toBe("password");
    const tokenToggle = screen.getByRole("button", { name: "Mostrar token" });
    await user.click(tokenToggle);
    expect(tokenInput.type).toBe("text");
    expect(screen.getByRole("button", { name: "Ocultar token" })).toBeTruthy();
  });
});

afterAll(() => {
  globalThis.fetch = originalFetch;
});
