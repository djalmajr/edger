import { ensureConsoleUsersDom } from "./console-users.dom";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import type { Principal } from "../lib/api";
import type { AdminSession } from "./console-sessions";

const { cleanup, render, screen, waitFor, within } = await import(
  "@testing-library/react"
);
const { default: userEvent } = await import("@testing-library/user-event");
const { QueryClient, QueryClientProvider } = await import("@tanstack/react-query");
const { I18nProvider } = await import("../lib/i18n");
const { ConsoleSessions } = await import("./console-sessions");

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

function makeSession(overrides: Partial<AdminSession> = {}): AdminSession {
  return {
    createdAt: 1_700_000_000,
    current: false,
    expiresAt: 1_700_864_000,
    id: "sess-1",
    isRoot: false,
    role: "operator",
    userId: 2,
    username: "analyst-01",
    ...overrides,
  };
}

const sessionBody = {
  sessions: [
    makeSession({
      current: true,
      id: "sess-current",
      isRoot: true,
      role: "root",
      userId: 1,
      username: "root",
    }),
    makeSession(),
  ],
  truncated: false,
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

function renderSessions(principal: Principal = rootPrincipal) {
  localStorage.setItem("edger.cpanel.locale", "en-US");
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <I18nProvider>
        <ConsoleSessions apiKey="ses-root-abc" principal={principal} />
      </I18nProvider>
    </QueryClientProvider>,
  );
}

// The root fixture repeats its username in the role cell, so the first
// match in DOM order (the username cell) addresses the right row.
function rowOf(username: string) {
  return screen.getAllByText(username)[0].closest("tr") as HTMLElement;
}

describe("ConsoleSessions", () => {
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
    renderSessions(operatorPrincipal);
    expect(
      screen.getByText(
        "Only the root principal manages sessions. Ask the operator for access.",
      ),
    ).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("lists the active sessions with a fixed elastic username column", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, sessionBody);
    renderSessions();
    const table = await screen.findByRole("table");
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    // The root session shows its username and its role, both "root".
    expect(within(table).getAllByText("root")).toHaveLength(2);
    expect(within(table).getByText("analyst-01")).toBeTruthy();
    expect(within(table).getByText("Current session")).toBeTruthy();
    // The grid is fixed-layout with the username column elastic: it takes
    // the leftover width while the dated columns keep their px.
    expect(table.style.tableLayout).toBe("fixed");
    expect(table.style.minWidth).toBe("720px");
    expect(
      (table.querySelector("thead th:first-child") as HTMLElement).style.width,
    ).toBe("");
    expect(
      (table.querySelector("thead th:nth-child(3)") as HTMLElement).style.width,
    ).toBe("154px");
  });

  it("filters by username or role case-insensitively and clears to everything", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, sessionBody);
    renderSessions();
    const table = await screen.findByRole("table");
    const search = screen.getByRole("textbox", { name: "Search sessions" });
    const user = userEvent.setup();
    // Trimmed and case-insensitive over the username field.
    await user.type(search, "  ANALYST ");
    expect(within(table).getAllByRole("row")).toHaveLength(2);
    expect(within(table).getByText("analyst-01")).toBeTruthy();
    expect(within(table).queryByText("root")).toBeNull();
    // The role field is searched too.
    await user.clear(search);
    await user.type(search, "OPERATOR");
    expect(within(table).getAllByRole("row")).toHaveLength(2);
    await user.clear(search);
    expect(within(table).getAllByRole("row")).toHaveLength(3);
  });

  it("disables the revoke action on the current session with the logout hint", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, sessionBody);
    renderSessions();
    await screen.findByText("analyst-01");
    const currentButton = within(rowOf("root")).getByRole("button", {
      name: "Revoke",
    });
    expect(currentButton.hasAttribute("disabled")).toBe(true);
    expect(currentButton.getAttribute("title")).toBe(
      "To end this session, use Log out.",
    );
    expect(
      within(rowOf("analyst-01"))
        .getByRole("button", { name: "Revoke" })
        .hasAttribute("disabled"),
    ).toBe(false);
  });

  it("revokes only after confirmation and removes the row after the refetch", async () => {
    let body = { sessions: [...sessionBody.sessions], truncated: false };
    fetchMock.mockImplementation(
      async (_input: RequestInfo | URL, init?: RequestInit) => {
        if (init?.method === "DELETE") {
          // The server drops the session; the next list fetch no longer has it.
          body = {
            sessions: body.sessions.filter((session) => session.id !== "sess-1"),
            truncated: false,
          };
          return jsonResponse({ ok: true });
        }
        return jsonResponse(body);
      },
    );
    globalThis.fetch = fetchMock;
    renderSessions();
    const table = await screen.findByRole("table");
    const user = userEvent.setup();
    // Opening the dialog does not delete yet.
    await user.click(within(rowOf("analyst-01")).getByRole("button", { name: "Revoke" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain(
      "The session for analyst-01 ends immediately",
    );
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "DELETE")
        .length,
    ).toBe(0);
    // The confirmed revoke issues the DELETE once, closes the dialog, and
    // the invalidated query refetches the list without the revoked session.
    await user.click(within(rowOf("analyst-01")).getByRole("button", { name: "Revoke" }));
    const confirmDialog = await screen.findByRole("dialog");
    await user.click(
      within(confirmDialog).getByRole("button", { name: "Revoke" }),
    );
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE"),
      ).toBe(true),
    );
    const del = fetchMock.mock.calls.find(
      ([, init]) => init?.method === "DELETE",
    ) as [string, RequestInit];
    expect(String(del[0])).toContain("/api/admin/sessions/sess-1");
    expect(screen.queryByRole("dialog")).toBeNull();
    await waitFor(() => expect(within(table).queryByText("analyst-01")).toBeNull());
    expect(within(table).getAllByText("root")).toHaveLength(2);
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "DELETE")
        .length,
    ).toBe(1);
  });

  it("encodes a hostile session id into the DELETE path", async () => {
    fetchMock.mockImplementation(
      async (_input: RequestInfo | URL, init?: RequestInit) =>
        init?.method === "DELETE"
          ? jsonResponse({ ok: true })
          : jsonResponse({
              sessions: [makeSession({ id: "a b/c" })],
              truncated: false,
            }),
    );
    globalThis.fetch = fetchMock;
    renderSessions();
    const user = userEvent.setup();
    await screen.findByText("analyst-01");
    await user.click(within(rowOf("analyst-01")).getByRole("button", { name: "Revoke" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Revoke" }));
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE"),
      ).toBe(true),
    );
    const del = fetchMock.mock.calls.find(
      ([, init]) => init?.method === "DELETE",
    ) as [string, RequestInit];
    expect(String(del[0])).toBe("http://localhost/api/admin/sessions/a%20b%2Fc");
  });

  it("keeps the row and shows the error when the revoke fails", async () => {
    fetchMock.mockImplementation(
      async (_input: RequestInfo | URL, init?: RequestInit) =>
        init?.method === "DELETE"
          ? jsonResponse({ code: "NOT_FOUND", message: "session not found" }, 404)
          : jsonResponse(sessionBody),
    );
    globalThis.fetch = fetchMock;
    renderSessions();
    const table = await screen.findByRole("table");
    const user = userEvent.setup();
    await user.click(within(rowOf("analyst-01")).getByRole("button", { name: "Revoke" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Revoke" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("session not found");
    // The failed delete keeps the row and the dialog open, and exactly one
    // DELETE was attempted — no silent drop, no retry storm.
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(within(table).getByText("analyst-01")).toBeTruthy();
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method === "DELETE")
        .length,
    ).toBe(1);
  });

  it("distinguishes the empty list from a search with no match", async () => {
    globalThis.fetch = fetchMock;
    stubJson(fetchMock, { sessions: [makeSession()], truncated: false });
    renderSessions();
    const search = await screen.findByRole("textbox", { name: "Search sessions" });
    const user = userEvent.setup();
    await user.type(search, "no-such-user");
    expect(
      screen.getByText("No sessions match the search."),
    ).toBeTruthy();
    await user.clear(search);
    await screen.findByText("analyst-01");
    // With an empty list the generic text comes back.
    fetchMock.mockImplementation(async () =>
      jsonResponse({ sessions: [], truncated: false }),
    );
    await user.click(screen.getByRole("button", { name: "Refresh" }));
    await screen.findByText("No active sessions.");
  });

  it("announces the truncated list and refetches on the refresh button", async () => {
    globalThis.fetch = fetchMock;
    fetchMock.mockImplementation(async () =>
      jsonResponse({ sessions: [makeSession()], truncated: true }),
    );
    renderSessions();
    await screen.findByText("analyst-01");
    expect(
      screen.getByText("List limited to the 1000 most recent sessions."),
    ).toBeTruthy();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
  });

  it("shows the translated loading, query error and retry recovery", async () => {
    let resolveList!: (value: Response) => void;
    fetchMock.mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          resolveList = resolve;
        }),
    );
    globalThis.fetch = fetchMock;
    renderSessions();
    expect(screen.getByRole("status").textContent).toBe("Loading sessions");

    resolveList(jsonResponse({ sessions: [], truncated: false }));
    await screen.findByText("No active sessions.");

    // A fresh render that fails shows the error line plus a working retry.
    cleanup();
    let fail = true;
    fetchMock.mockImplementation(async () =>
      fail
        ? jsonResponse({ code: "ERR", message: "boom" }, 500)
        : jsonResponse({ sessions: [makeSession()], truncated: false }),
    );
    const client = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    });
    render(
      <QueryClientProvider client={client}>
        <I18nProvider>
          <ConsoleSessions apiKey="ses-root-abc" principal={rootPrincipal} />
        </I18nProvider>
      </QueryClientProvider>,
    );
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("The active sessions could not be read.");
    fail = false;
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("analyst-01");
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

afterAll(() => {
  globalThis.fetch = originalFetch;
});
