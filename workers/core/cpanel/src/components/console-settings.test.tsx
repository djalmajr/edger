import { ensureConsoleUsersDom } from "./console-users.dom";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import type { Principal } from "../lib/api";

const { cleanup, render, screen, waitFor } = await import(
  "@testing-library/react"
);
const { default: userEvent } = await import("@testing-library/user-event");
const { QueryClient, QueryClientProvider } = await import("@tanstack/react-query");
const { I18nProvider } = await import("../lib/i18n");
const { ConsoleSettings } = await import("./console-settings");

const originalFetch = globalThis.fetch;
const fetchMock = vi.fn();

const rootPrincipal: Principal = {
  isRoot: true,
  name: "root",
  role: "root",
};
const operatorPrincipal: Principal = {
  isRoot: false,
  name: "analyst",
  role: "operator",
  permissions: ["workers:read"],
};

const settingsBody = {
  loginMaxFailures: 5,
  loginWindowSeconds: 300,
  passwordEnabled: true,
  passwordMaxLength: 64,
  passwordMinLength: 8,
  runtimeVersion: "0.9.1",
  sessionTtlSeconds: 2_592_000,
  tenantRoutingEnabled: true,
  weightedRoutingEnabled: false,
};

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
    status,
  });
}

// Persistent fetch stubs must build a FRESH Response per call: reusing one
// Response object makes the second consumer throw "body already used".
function stubJson(
  fetchMock: ReturnType<typeof vi.fn>,
  body: unknown,
  status = 200,
) {
  fetchMock.mockImplementation(async () => jsonResponse(body, status));
}

function renderSettings(principal: Principal = rootPrincipal) {
  localStorage.setItem("edger.cpanel.locale", "en-US");
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <I18nProvider>
        <ConsoleSettings apiKey="ses-root-abc" principal={principal} />
      </I18nProvider>
    </QueryClientProvider>,
  );
}

describe("ConsoleSettings", () => {
  beforeAll(() => {
    // bun evaluates all test files' modules before any test runs; a sibling
    // file's afterAll may have dropped the shared DOM globals in between.
    ensureConsoleUsersDom();
  });
  afterEach(() => {
    cleanup();
    fetchMock.mockReset();
    globalThis.fetch = originalFetch;
    localStorage.clear();
    sessionStorage.clear();
  });

  it("shows only the notice to a non-root principal and does not fetch", () => {
    globalThis.fetch = fetchMock;
    renderSettings(operatorPrincipal);
    expect(
      screen.getByText(
        "Only the root principal views settings. Ask the operator for access.",
      ),
    ).toBeTruthy();
    expect(screen.queryByRole("heading")).toBeNull();
    expect(screen.queryByRole("button")).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("shows every real field as read-only values without any write control", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, settingsBody);
    renderSettings();
    // Runtime.
    expect(await screen.findByText("Runtime")).toBeTruthy();
    expect(screen.getByText("Runtime version")).toBeTruthy();
    expect(screen.getByText("0.9.1")).toBeTruthy();
    // Routing flags with the localized enabled/disabled values.
    expect(screen.getByText("Routing")).toBeTruthy();
    expect(screen.getByText("Tenant routing")).toBeTruthy();
    expect(screen.getAllByText("Enabled")).toHaveLength(2);
    expect(screen.getByText("Weighted routing")).toBeTruthy();
    expect(screen.getByText("Disabled")).toBeTruthy();
    // Security.
    expect(screen.getByText("Security")).toBeTruthy();
    expect(screen.getByText("Password sign-in")).toBeTruthy();
    expect(screen.getByText("Session duration")).toBeTruthy();
    expect(screen.getByText("30 days")).toBeTruthy();
    expect(screen.getByText("Password length")).toBeTruthy();
    expect(screen.getByText("8–64")).toBeTruthy();
    expect(screen.getByText("Login window")).toBeTruthy();
    expect(screen.getByText("5 minutes")).toBeTruthy();
    expect(screen.getByText("Max login failures")).toBeTruthy();
    expect(screen.getByText("5")).toBeTruthy();
    // The read-only notice is present...
    expect(
      screen.getByText(
        "This installation's settings are defined at startup. This page is read-only.",
      ),
    ).toBeTruthy();
    // ...and no input, switch, checkbox or save control exists to write.
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.queryByRole("switch")).toBeNull();
    expect(screen.queryByRole("button", { name: "Save" })).toBeNull();
  });

  it("formats singular day and minute values", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, {
      ...settingsBody,
      loginWindowSeconds: 60,
      sessionTtlSeconds: 86_400,
    });
    renderSettings();
    await screen.findByText("1 day");
    expect(screen.getByText("1 minute")).toBeTruthy();
  });

  it("shows the translated loading, query error and retry recovery", async () => {
    let resolveSettings!: (value: Response) => void;
    fetchMock.mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          resolveSettings = resolve;
        }),
    );
    globalThis.fetch = fetchMock;
    renderSettings();
    expect(screen.getByRole("status").textContent).toBe("Loading settings");

    resolveSettings(jsonResponse(settingsBody));
    await screen.findByText("0.9.1");

    // A fresh render that fails shows the error line plus a working retry.
    cleanup();
    let fail = true;
    fetchMock.mockImplementation(async () =>
      fail
        ? jsonResponse({ code: "ERR", message: "boom" }, 500)
        : jsonResponse(settingsBody),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    render(
      <QueryClientProvider client={client}>
        <I18nProvider>
          <ConsoleSettings apiKey="ses-root-abc" principal={rootPrincipal} />
        </I18nProvider>
      </QueryClientProvider>,
    );
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("The settings could not be read.");
    fail = false;
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("0.9.1");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("refetches on the refresh button", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, settingsBody);
    renderSettings();
    await screen.findByText("0.9.1");
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
  });
});

afterAll(() => {
  globalThis.fetch = originalFetch;
});
