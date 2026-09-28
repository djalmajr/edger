import { ensureConsoleUsersDom } from "./console-users.dom";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import * as React from "react";
import type { AdminUser, Principal } from "../lib/api";

const { cleanup, render, screen, waitFor, within } = await import(
  "@testing-library/react"
);
const { default: userEvent } = await import("@testing-library/user-event");
const { QueryClient, QueryClientProvider } = await import("@tanstack/react-query");
const { I18nProvider } = await import("../lib/i18n");
const { SESSION_KEY } = await import("../lib/api");
const { ChangePasswordDialog, ConsoleUsers } = await import("./console-users");

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

function makeUser(overrides: Partial<AdminUser> = {}): AdminUser {
  return {
    createdAt: 1700000000,
    disabled: false,
    id: 2,
    isRoot: false,
    namespaces: ["*"],
    permissions: ["workers:read", "files:read"],
    role: "operator",
    username: "analyst-01",
    workers: ["*"],
    ...overrides,
  };
}

const listBody = {
  users: [
    makeUser({ id: 1, isRoot: true, permissions: ["*"], role: "root", username: "root" }),
    makeUser(),
  ],
};

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
    status,
  });
}

// Persistent fetch stubs must build a FRESH Response per call: reusing one
// Response object makes the second consumer throw "body already used".
function stubJson(fetchMock: ReturnType<typeof vi.fn>, body: unknown, status = 200) {
  fetchMock.mockImplementation(async () => jsonResponse(body, status));
}

function renderUsers(principal: Principal = rootPrincipal) {
  localStorage.setItem("edger.cpanel.locale", "en-US");
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <I18nProvider>
        <ConsoleUsers apiKey="ses-root-abc" principal={principal} />
      </I18nProvider>
    </QueryClientProvider>,
  );
}

function renderChangePassword(
  onNewSession: (token: string) => void,
  onRequireLogin: () => void,
) {
  localStorage.setItem("edger.cpanel.locale", "en-US");
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  // A tiny harness owns the open state exactly like the account menu does,
  // so a successful change actually closes the dialog.
  function Harness() {
    const [open, setOpen] = React.useState(true);
    return (
      <ChangePasswordDialog
        apiKey="ses-current"
        onNewSession={onNewSession}
        onOpenChange={setOpen}
        onRequireLogin={onRequireLogin}
        open={open}
      />
    );
  }
  return render(
    <QueryClientProvider client={client}>
      <I18nProvider>
        <Harness />
      </I18nProvider>
    </QueryClientProvider>,
  );
}

describe("ConsoleUsers", () => {
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
    renderUsers(operatorPrincipal);
    expect(
      screen.getByText("Only the root principal manages users. Ask the operator for access."),
    ).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
    expect(screen.queryByRole("button", { name: "New user" })).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("lists users with status, permissions and scopes under the root session", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, listBody);
    renderUsers();
    await screen.findByText("analyst-01");
    // The root row is visible but immutable: badge, no action buttons.
    expect(screen.getAllByText("root").length).toBeGreaterThanOrEqual(2);
    // Both users are enabled: one Active badge each.
    expect(screen.getAllByText("Active").length).toBe(2);
    expect(screen.getByText("workers:read")).toBeTruthy();
    expect(screen.getByText("files:read")).toBeTruthy();
    // One row per non-root user, with the four actions.
    expect(screen.getByRole("button", { name: "Edit" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Disable" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Reset password" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Delete" })).toBeTruthy();
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(String(url)).toContain("/api/admin/users");
    // No explicit method: the fetch defaults to GET.
    expect(init.method ?? "GET").toBe("GET");
    expect(new Headers(init.headers).get("x-api-key")).toBe("ses-root-abc");
  });

  it("surfaces a failed list read with a retry that recovers", async () => {
    globalThis.fetch = fetchMock;
    fetchMock.mockRejectedValueOnce(new TypeError("fetch failed"));
    renderUsers();
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("fetch failed");
    const user = userEvent.setup();
    stubJson(fetchMock, listBody);
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("analyst-01");
  });

  it("renders the empty list with the create invitation", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, { users: [] });
    renderUsers();
    expect(
      await screen.findByText(
        "No additional users yet. Create the first one to delegate console access.",
      ),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "New user" })).toBeTruthy();
  });

  it("creates a user with the requested grants and clears the form on success", async () => {
    globalThis.fetch = fetchMock;
    const createdUser = makeUser({
      id: 3,
      permissions: ["workers:read", "keys:manage"],
      username: "analyst-02",
    });
    fetchMock
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse({ user: createdUser }, 201));
    stubJson(fetchMock, listBody);
    renderUsers();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "New user" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText("Username"), "analyst-02");
    await user.type(within(dialog).getByLabelText("Password"), "str0ng!passw0rd");
    await user.click(within(dialog).getByRole("checkbox", { name: "keys:manage" }));
    await user.click(within(dialog).getByRole("button", { name: "New user" }));
    await waitFor(() =>
      expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST")).toBe(
        true,
      ),
    );
    const create = fetchMock.mock.calls.find(
      ([, init]) => init?.method === "POST",
    ) as unknown as [string, RequestInit];
    expect(String(create[0])).toContain("/api/admin/users");
    expect(new Headers(create[1].headers).get("x-api-key")).toBe("ses-root-abc");
    expect(JSON.parse(String(create[1].body))).toEqual({
      namespaces: ["*"],
      password: "str0ng!passw0rd",
      permissions: ["workers:read", "keys:manage"],
      username: "analyst-02",
      workers: ["*"],
    });
    // Success closes the dialog; the password-shaped value is gone from the
    // document and the list refetches for the new user.
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.body.textContent).not.toContain("str0ng!passw0rd");
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.filter(
          ([url]) => String(url).endsWith("/api/admin/users"),
        ).length,
      ).toBeGreaterThanOrEqual(2),
    );
  });

  it("keeps the create form with its values when the server rejects", async () => {
    globalThis.fetch = fetchMock;
    fetchMock
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(
        jsonResponse({ code: "USERNAME_TAKEN", message: "username already exists" }, 409),
      );
    renderUsers();    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "New user" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText("Username"), "analyst-01");
    await user.type(within(dialog).getByLabelText("Password"), "str0ng!passw0rd");
    await user.click(within(dialog).getByRole("button", { name: "New user" }));
    // The error is visible and the form keeps both values.
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("username already exists");
    expect(
      (within(screen.getByRole("dialog")).getByLabelText("Username") as HTMLInputElement).value,
    ).toBe("analyst-01");
    expect(
      (within(screen.getByRole("dialog")).getByLabelText("Password") as HTMLInputElement).value,
    ).toBe("str0ng!passw0rd");
  });

  it("refuses an invalid username and a weak password before any request", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, listBody);
    renderUsers();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "New user" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText("Username"), "A");
    expect(
      within(dialog).getByText(
        /Invalid name: use lowercase letters, digits, dot, underscore and hyphen/,
      ),
    ).toBeTruthy();
    await user.type(within(dialog).getByLabelText("Password"), "weak");
    expect(
      within(dialog).getByText(
        /must be 12 to 128 characters and include a letter, a digit and a symbol/,
      ),
    ).toBeTruthy();
    expect(
      within(dialog).getByRole("button", { name: "New user" }),
    ).toHaveProperty("disabled", true);
    // A valid pair re-enables the submit without touching the network.
    const usernameInput = within(dialog).getByLabelText("Username");
    const passwordInput = within(dialog).getByLabelText("Password");
    await user.clear(usernameInput);
    await user.type(usernameInput, "analyst-02");
    await user.clear(passwordInput);
    await user.type(passwordInput, "tr0ng!passw0rd");
    expect(
      within(dialog).getByRole("button", { name: "New user" }),
    ).toHaveProperty("disabled", false);
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "POST").length,
    ).toBe(0);
  });

  it("confirms before disabling and re-enables without confirmation", async () => {
    globalThis.fetch = fetchMock;
    fetchMock
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(jsonResponse({ user: makeUser({ disabled: true }) }));
    stubJson(fetchMock, listBody);
    renderUsers();
    const user = userEvent.setup();
    await screen.findByText("analyst-01");
    await user.click(screen.getByRole("button", { name: "Disable" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain(
      "The user's active sessions are revoked and they cannot sign in until re-enabled.",
    );
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "PATCH").length,
    ).toBe(0);
    // Confirm path: the PATCH body only carries the disabled flag.
    await user.click(screen.getByRole("button", { name: "Disable" }));
    const again = await screen.findByRole("dialog");
    await user.click(within(again).getByRole("button", { name: "Confirm" }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([, init]) => init?.method === "PATCH"),
      ).toBe(true),
    );
    const patch = fetchMock.mock.calls.find(
      ([, init]) => init?.method === "PATCH",
    ) as unknown as [string, RequestInit];
    expect(String(patch[0])).toContain("/api/admin/users/2");
    expect(JSON.parse(String(patch[1].body))).toEqual({ disabled: true });
  });

  it("re-enables a disabled user directly with a PATCH", async () => {
    globalThis.fetch = fetchMock;
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse({ users: [makeUser({ disabled: true })] }),
      )
      .mockResolvedValueOnce(jsonResponse({ user: makeUser() }));
    stubJson(fetchMock, listBody);
    renderUsers();
    const te = userEvent.setup();
    await screen.findByText("analyst-01");
    expect(screen.getByText("Disabled")).toBeTruthy();
    await te.click(screen.getByRole("button", { name: "Enable" }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([, init]) => init?.method === "PATCH"),
      ).toBe(true),
    );
    const patch = fetchMock.mock.calls.find(
      ([, init]) => init?.method === "PATCH",
    ) as unknown as [string, RequestInit];
    expect(JSON.parse(String(patch[1].body))).toEqual({ disabled: false });
  });

  it("surfaces a revoked session on a mutation and keeps the confirmation", async () => {
    globalThis.fetch = fetchMock;
    fetchMock
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(
        jsonResponse(
          { code: "UNAUTHORIZED", message: "missing or invalid API key" },
          401,
        ),
      );
    renderUsers();
    const user = userEvent.setup();
    await screen.findByText("analyst-01");
    await user.click(screen.getByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain("This cannot be undone.");
    await user.click(within(dialog).getByRole("button", { name: "Confirm" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("missing or invalid API key");
    // The delete was attempted once, failed with 401, and the confirmation
    // stays open with the error — no silent drop, no retry storm.
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "DELETE").length,
    ).toBe(1);
  });

  it("deletes only after confirmation", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, listBody);
    renderUsers();
    const user = userEvent.setup();
    await screen.findByText("analyst-01");
    await user.click(screen.getByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "DELETE").length,
    ).toBe(0);
    await user.click(screen.getByRole("button", { name: "Delete" }));
    const again = await screen.findByRole("dialog");
    await user.click(within(again).getByRole("button", { name: "Confirm" }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE"),
      ).toBe(true),
    );
    const del = fetchMock.mock.calls.find(
      ([, init]) => init?.method === "DELETE",
    ) as unknown as [string, RequestInit];
    expect(String(del[0])).toContain("/api/admin/users/2");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("resets the password after confirmation and clears the value", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, listBody);
    renderUsers();
    const user = userEvent.setup();
    await screen.findByText("analyst-01");
    await user.click(screen.getByRole("button", { name: "Reset password" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain(
      "revokes all of the user's active sessions immediately",
    );
    // The dialog has a single input; happy-dom maps type=password without
    // the textbox role, so it is addressed by label.
    await user.type(within(dialog).getByLabelText("Password"), "an0ther!strongPass");
    await user.click(within(dialog).getByRole("button", { name: "Reset" }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([, init]) => init?.method === "POST" &&
          String(init.body).includes("an0ther!strongPass")),
      ).toBe(true),
    );
    const reset = fetchMock.mock.calls.find(
      ([url]) => String(url).includes("/reset-password"),
    ) as unknown as [string, RequestInit];
    expect(String(reset[0])).toContain("/api/admin/users/2/reset-password");
    expect(JSON.parse(String(reset[1].body))).toEqual({
      password: "an0ther!strongPass",
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.body.textContent).not.toContain("an0ther!strongPass");
  });

  it("keeps the reset form with its value when the server rejects", async () => {
    globalThis.fetch = fetchMock;
    fetchMock
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(
        jsonResponse(
          { code: "VALIDATION_ERROR", message: "password too weak" },
          400,
        ),
      );
    renderUsers();    const user = userEvent.setup();
    await screen.findByText("analyst-01");
    await user.click(screen.getByRole("button", { name: "Reset password" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(within(dialog).getByLabelText("Password"), "an0ther!strongPass");
    await user.click(within(dialog).getByRole("button", { name: "Reset" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("password too weak");
    expect(
      (
        within(screen.getByRole("dialog")).getByLabelText("Password") as HTMLInputElement
      ).value,
    ).toBe("an0ther!strongPass");
  });

  it("edits the grants and sends only the editable fields", async () => {
    globalThis.fetch = fetchMock;
    fetchMock
      .mockResolvedValueOnce(jsonResponse(listBody))
      .mockResolvedValueOnce(
        jsonResponse({ user: makeUser({ permissions: ["workers:read"] }) }),
      );
    stubJson(fetchMock, listBody);
    renderUsers();
    const user = userEvent.setup();
    await screen.findByText("analyst-01");
    await user.click(screen.getByRole("button", { name: "Edit" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).queryByLabelText("Password")).toBeNull();
    await user.click(within(dialog).getByRole("checkbox", { name: "files:read" }));
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([, init]) => init?.method === "PATCH"),
      ).toBe(true),
    );
    const patch = fetchMock.mock.calls.find(
      ([, init]) => init?.method === "PATCH",
    ) as unknown as [string, RequestInit];
    expect(JSON.parse(String(patch[1].body))).toEqual({
      namespaces: ["*"],
      permissions: ["workers:read"],
      workers: ["*"],
    });
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("ChangePasswordDialog", () => {
  beforeAll(() => {
    ensureConsoleUsersDom();
  });
  afterEach(() => {
    cleanup();
    fetchMock.mockReset();
    globalThis.fetch = originalFetch;
    localStorage.clear();
    sessionStorage.clear();
  });

  it("sends the current and the new password and rotates the stored session", async () => {
    globalThis.fetch = fetchMock;
    const onNewSession = vi.fn((token: string) =>
      sessionStorage.setItem(SESSION_KEY, token),
    );
    const onRequireLogin = vi.fn();
    renderChangePassword(onNewSession, onRequireLogin);
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Current password"), "old!Passw0rd123");
    await user.type(screen.getByLabelText("New password"), "new!Passw0rd123");
    stubJson(fetchMock, { token: "ses-new-token" });
    await user.click(screen.getByRole("button", { name: "Change password" }));
    await waitFor(() =>
      expect(onNewSession).toHaveBeenCalledWith("ses-new-token"),
    );
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("ses-new-token");
    expect(onRequireLogin).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
    const call = fetchMock.mock.calls.find(
      ([url]) => String(url).includes("/api/admin/me/password"),
    ) as unknown as [string, RequestInit];
    expect(call[1].method).toBe("POST");
    expect(new Headers(call[1].headers).get("x-api-key")).toBe("ses-current");
    expect(JSON.parse(String(call[1].body))).toEqual({
      current: "old!Passw0rd123",
      new: "new!Passw0rd123",
    });
    // Neither password survives in the document after the change.
    expect(document.body.textContent).not.toContain("old!Passw0rd123");
    expect(document.body.textContent).not.toContain("new!Passw0rd123");
  });

  it("ends the local session when the answer carries no new token", async () => {
    globalThis.fetch = fetchMock;
    const onNewSession = vi.fn();
    const onRequireLogin = vi.fn(() => sessionStorage.removeItem(SESSION_KEY));
    renderChangePassword(onNewSession, onRequireLogin);
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Current password"), "old!Passw0rd123");
    await user.type(screen.getByLabelText("New password"), "new!Passw0rd123");
    stubJson(fetchMock, {});
    await user.click(screen.getByRole("button", { name: "Change password" }));
    await waitFor(() => expect(onRequireLogin).toHaveBeenCalled());
    expect(onNewSession).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
  });

  it("keeps the form with both values when the current password is rejected", async () => {
    globalThis.fetch = fetchMock;
    const onNewSession = vi.fn();
    const onRequireLogin = vi.fn();
    renderChangePassword(onNewSession, onRequireLogin);
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("Current password"), "old!Passw0rd123");
    await user.type(screen.getByLabelText("New password"), "new!Passw0rd123");
    stubJson(fetchMock, { message: "invalid credentials" }, 401);
    await user.click(screen.getByRole("button", { name: "Change password" }));
    expect(
      await screen.findByText("The current password is not valid."),
    ).toBeTruthy();
    // The error copy must not echo the server body.
    expect(document.body.textContent).not.toContain("invalid credentials");
    expect(
      (screen.getByLabelText("Current password") as HTMLInputElement).value,
    ).toBe("old!Passw0rd123");
    expect(
      (screen.getByLabelText("New password") as HTMLInputElement).value,
    ).toBe("new!Passw0rd123");
    expect(onNewSession).not.toHaveBeenCalled();
    expect(onRequireLogin).not.toHaveBeenCalled();
  });
});

afterAll(() => {
  globalThis.fetch = originalFetch;
});
