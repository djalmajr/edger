import "./routing-policy.dom";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Principal, Worker } from "../lib/api";

const { cleanup, render, screen, waitFor, within } = await import(
  "@testing-library/react"
);
const { default: userEvent } = await import("@testing-library/user-event");
const { QueryClient, QueryClientProvider } = await import("@tanstack/react-query");
const { I18nProvider } = await import("../lib/i18n");
const { RoutingPolicyPanel } = await import("./routing-policy");

const root: Principal = { isRoot: true, permissions: ["workers:read"] };
const reader: Principal = { isRoot: false, permissions: ["workers:read"] };

function worker(overrides: Partial<Worker> = {}): Worker {
  return {
    kind: "fullstack",
    name: "shop",
    origin: "user",
    status: "loaded",
    version: "1.0.0",
    visibility: "public",
    ...overrides,
  };
}

const catalog = [
  worker({ defaultVersion: "1.0.0", version: "2.0.0" }),
  worker({ defaultVersion: "1.0.0", version: "1.0.0" }),
  worker({ staged: true, version: "1.9.0" }),
  worker({ version: "1.8.0", visibility: "internal" }),
  worker({ status: "disabled", version: "1.7.0" }),
  worker({ origin: "core_bundled", version: "1.6.0" }),
];

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
    status,
  });
}

function renderPanel(
  versions: Worker[] = catalog,
  principal: Principal = root,
) {
  localStorage.setItem("edger.cpanel.locale", "en-US");
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <I18nProvider>
        <RoutingPolicyPanel
          apiKey="root-key"
          principal={principal}
          versions={versions}
        />
      </I18nProvider>
    </QueryClientProvider>,
  );
}

function requestedUrl(call: unknown[]): URL {
  return new URL(String(call[0]), "http://localhost");
}

describe("routing policy panel", () => {
  const fetchMock = vi.fn();

  const originalFetch = globalThis.fetch;

  afterEach(() => {
    cleanup();
    globalThis.fetch = originalFetch;
  });

  function stubPolicy(body: unknown, status = 200) {
    fetchMock.mockReset();
    fetchMock.mockResolvedValue(jsonResponse(body, status));
    globalThis.fetch = fetchMock;
    localStorage.setItem("edger.cpanel.locale", "en-US");
  }

  it("explains host availability and session cohorts before a policy exists", async () => {
    stubPolicy({ policy: null });
    renderPanel();
    expect(await screen.findByText(/does not authenticate the visitor/)).toBeTruthy();
    expect(screen.getByText(/80\/20/)).toBeTruthy();
    expect(screen.getByText(/Rancher or install setup/)).toBeTruthy();
    expect(screen.getByText(/does not say whether they are on/)).toBeTruthy();
    expect(screen.getAllByText(/Tenancit is not configured/).length).toBeGreaterThan(0);
    expect(screen.getByText(/empty tenant list is not a substitute/)).toBeTruthy();
    expect(screen.getByText("1.0.0", { selector: "span" })).toBeTruthy();
    expect(fetchMock).toHaveBeenCalled();
    const url = requestedUrl(fetchMock.mock.calls[0]);
    expect(url.pathname.endsWith("/api/admin/routing-policy")).toBe(true);
    expect(url.searchParams.get("name")).toBe("shop");
  });

  it("shows a loading status and then an error that can be retried", async () => {
    let resolveFetch: (response: Response) => void = () => {};
    fetchMock.mockReset();
    fetchMock.mockImplementation(
      () =>
        new Promise<Response>((resolve) => {
          resolveFetch = resolve;
        }),
    );
    globalThis.fetch = fetchMock;
    localStorage.setItem("edger.cpanel.locale", "en-US");
    renderPanel([worker()]);
    expect(screen.getByRole("status").textContent).toContain(
      "Loading routing policy",
    );
    resolveFetch(jsonResponse({ message: "denied" }, 403));
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "denied",
    );
    fetchMock.mockResolvedValueOnce(jsonResponse({ policy: null }));
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText(/No routing policy is stored/)).toBeTruthy();
  });

  it("does not offer staged, internal, disabled, or core versions", async () => {
    stubPolicy({ policy: null });
    const user = userEvent.setup();
    renderPanel();
    await screen.findByText(/No routing policy is stored/);
    await user.click(
      screen.getByRole("checkbox", { name: "Distribute sessions across versions" }),
    );
    expect(screen.getByRole("checkbox", { name: "2.0.0" })).toBeTruthy();
    expect(screen.getByRole("checkbox", { name: "1.0.0" })).toBeTruthy();
    expect(screen.queryByRole("checkbox", { name: "1.9.0" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "1.8.0" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "1.7.0" })).toBeNull();
    expect(screen.queryByRole("checkbox", { name: "1.6.0" })).toBeNull();
    expect(screen.getByText(/1\.9\.0/)).toBeTruthy();
    expect(screen.getByText(/staged, not offered/)).toBeTruthy();
    expect(screen.getByText(/internal, not offered/)).toBeTruthy();
    expect(screen.getByText(/disabled, not offered/)).toBeTruthy();
    expect(screen.getByText(/core, not offered/)).toBeTruthy();
  });

  it("keeps a non-root principal on a read-only policy", async () => {
    stubPolicy({
      policy: {
        name: "shop",
        tenantAccess: { mode: "allowlist", tenants: ["acme"] },
      },
    });
    renderPanel(catalog, reader);
    expect(await screen.findByDisplayValue("acme")).toBeTruthy();
    expect(screen.getByLabelText("Tenant slug 1")).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: "Save configuration" })).toHaveProperty(
      "disabled",
      true,
    );
    expect(screen.getByRole("button", { name: "Remove configuration" })).toHaveProperty(
      "disabled",
      true,
    );
    expect(screen.getByText(/Only the root principal/)).toBeTruthy();
  });

  it("saves an 80/20 split only after confirmation", async () => {
    stubPolicy({ policy: null });
    const user = userEvent.setup();
    renderPanel([
      worker({ version: "2.0.0" }),
      worker({ version: "1.0.0" }),
    ]);
    await screen.findByText(/No routing policy is stored/);
    await user.click(
      screen.getByRole("checkbox", { name: "Distribute sessions across versions" }),
    );
    await user.click(screen.getByRole("checkbox", { name: "2.0.0" }));
    await user.click(screen.getByRole("checkbox", { name: "1.0.0" }));
    await user.type(screen.getByRole("textbox", { name: "Weight 2.0.0" }), "80");
    await user.type(screen.getByRole("textbox", { name: "Weight 1.0.0" }), "20");
    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain("not each request");
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(fetchMock.mock.calls.some((call) => call[1]?.method === "PUT")).toBe(
      false,
    );
    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    const again = await screen.findByRole("dialog");
    fetchMock.mockResolvedValueOnce(
      jsonResponse({
        policy: {
          name: "shop",
          tenantAccess: { mode: "public" },
          traffic: {
            versions: [
              { version: "2.0.0", weight: 80 },
              { version: "1.0.0", weight: 20 },
            ],
          },
        },
      }),
    );
    await user.click(within(again).getByRole("button", { name: "Save configuration" }));
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toContain("Policy saved");
    });
    const put = fetchMock.mock.calls.find((call) => call[1]?.method === "PUT");
    expect(put).toBeTruthy();
    expect(new Headers(put?.[1].headers).get("x-api-key")).toBe("root-key");
    expect(JSON.parse(String(put?.[1].body))).toEqual({
      name: "shop",
      tenantAccess: { mode: "public" },
      traffic: {
        versions: [
          { version: "2.0.0", weight: 80 },
          { version: "1.0.0", weight: 20 },
        ],
      },
    });
  });

  it("does not open confirmation when the weights do not total 100", async () => {
    stubPolicy({ policy: null });
    const user = userEvent.setup();
    renderPanel([worker({ version: "2.0.0" }), worker({ version: "1.0.0" })]);
    await screen.findByText(/No routing policy is stored/);
    await user.click(
      screen.getByRole("checkbox", { name: "Distribute sessions across versions" }),
    );
    await user.click(screen.getByRole("checkbox", { name: "2.0.0" }));
    await user.click(screen.getByRole("checkbox", { name: "1.0.0" }));
    await user.type(screen.getByRole("textbox", { name: "Weight 2.0.0" }), "50");
    await user.type(screen.getByRole("textbox", { name: "Weight 1.0.0" }), "40");
    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.getByRole("alert").textContent).toContain("must total 100");
  });

  it("confirms an allowlist before saving and does not send traffic", async () => {
    stubPolicy({ policy: null });
    const user = userEvent.setup();
    renderPanel([worker()]);
    await screen.findByText(/No routing policy is stored/);
    await user.click(screen.getByRole("radio", { name: "Tenant list" }));
    await user.type(screen.getByLabelText("Tenant slug 1"), "acme");
    await user.click(screen.getByRole("button", { name: "Save configuration" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain("does not authenticate visitors");
    fetchMock.mockResolvedValueOnce(
      jsonResponse({
        policy: {
          name: "shop",
          tenantAccess: { mode: "allowlist", tenants: ["acme"] },
        },
      }),
    );
    await user.click(within(dialog).getByRole("button", { name: "Save configuration" }));
    await screen.findByText(/Policy saved/);
    const put = fetchMock.mock.calls.find((call) => call[1]?.method === "PUT");
    expect(JSON.parse(String(put?.[1].body))).toEqual({
      name: "shop",
      tenantAccess: { mode: "allowlist", tenants: ["acme"] },
    });
  });

  it("confirms deletion as a return to the default version", async () => {
    stubPolicy({
      policy: {
        name: "shop",
        tenantAccess: { mode: "allowlist", tenants: ["acme"] },
        traffic: { versions: [{ version: "1.0.0", weight: 100 }] },
      },
    });
    const user = userEvent.setup();
    renderPanel([worker()]);
    await screen.findByText("Configured policy");
    await user.click(screen.getByRole("button", { name: "Remove configuration" }));
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain("not an empty tenant list");
    expect(dialog.textContent).toContain("does not authenticate users");
    fetchMock.mockResolvedValueOnce(jsonResponse({ deleted: true }));
    await user.click(within(dialog).getByRole("button", { name: "Remove configuration" }));
    await screen.findByText(/Configuration removed/);
    const deleted = fetchMock.mock.calls.find((call) => call[1]?.method === "DELETE");
    expect(deleted).toBeTruthy();
    expect(requestedUrl(deleted ?? []).searchParams.get("name")).toBe("shop");
  });

  it("requests the full manifest name for a namespaced app", async () => {
    stubPolicy({ policy: null });
    renderPanel([
      worker({ name: "@acme/shop", namespace: "@acme", version: "1.0.0" }),
    ]);
    await screen.findByText(/No routing policy is stored/);
    expect(requestedUrl(fetchMock.mock.calls[0]).searchParams.get("name")).toBe(
      "@acme/shop",
    );
  });

  it("does not call the API for a core app", () => {
    fetchMock.mockReset();
    globalThis.fetch = fetchMock;
    localStorage.setItem("edger.cpanel.locale", "en-US");
    renderPanel([worker({ name: "cpanel", origin: "core_bundled" })]);
    expect(screen.getByText(/do not take a routing policy/)).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
