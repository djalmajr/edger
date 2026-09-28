import * as React from "react";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import type { ApiKey, Principal } from "../lib/api";
import { I18nProvider, type Locale } from "../lib/i18n";

const nodeRequire = createRequire(import.meta.url);
const bunTest = (process.versions as { bun?: string }).bun
  ? (nodeRequire("bun:test") as {
      mock: { module(id: string, factory: () => Record<string, unknown>): void };
    })
  : undefined;

function element(tag: string) {
  return ({ children, ...props }: Record<string, unknown>) =>
    React.createElement(tag, props, children as React.ReactNode);
}

bunTest?.mock.module("@edger/ui/components/ui/button", () => ({
  Button: ({ children, size: _size, variant: _variant, ...props }: Record<string, unknown>) =>
    React.createElement("button", { type: "button", ...props }, children as React.ReactNode),
}));
bunTest?.mock.module("@edger/ui/components/ui/combobox", () => ({
  Combobox: ({
    "aria-label": ariaLabel,
    onValueChange,
    options,
    value,
  }: {
    "aria-label"?: string;
    onValueChange(value: string): void;
    options: Array<{ label: string; value: string }>;
    value: string;
  }) =>
    React.createElement(
      "select",
      {
        "aria-label": ariaLabel,
        onChange: (event: React.ChangeEvent<HTMLSelectElement>) =>
          onValueChange(event.target.value),
        value,
      },
      options.map((option) =>
        React.createElement("option", { key: option.value, value: option.value }, option.label),
      ),
    ),
}));
bunTest?.mock.module("@edger/ui/components/ui/dialog", () => {
  const Dialog = ({ children, open }: { children?: React.ReactNode; open?: boolean }) =>
    open ? React.createElement(React.Fragment, null, children) : null;
  const passthrough = (tag: string) => element(tag);
  return {
    Dialog,
    DialogContent: ({ children }: { children?: React.ReactNode }) =>
      React.createElement("div", { role: "dialog" }, children),
    DialogDescription: passthrough("p"),
    DialogFooter: passthrough("footer"),
    DialogHeader: passthrough("header"),
    DialogTitle: passthrough("h2"),
  };
});
bunTest?.mock.module("@edger/ui/components/ui/input", () => ({ Input: element("input") }));
bunTest?.mock.module("@edger/ui/components/ui/label", () => ({ Label: element("label") }));
bunTest?.mock.module("@edger/ui/components/ui/table", () => ({
  Table: element("table"),
  TableBody: element("tbody"),
  TableCell: element("td"),
  TableHead: element("th"),
  TableHeader: element("thead"),
  TableRow: element("tr"),
}));
bunTest?.mock.module("@edger/ui/components/ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: React.ReactNode }) =>
    React.createElement(React.Fragment, null, children),
  TooltipContent: () => null,
  TooltipTrigger: ({
    children,
    render,
  }: {
    children?: React.ReactNode;
    render?: React.ReactElement;
  }) =>
    render
      ? React.cloneElement(render, undefined, children)
      : React.createElement(React.Fragment, null, children),
}));
if (!("happyDOM" in globalThis)) {
  GlobalRegistrator.register({ url: "http://localhost/cpanel/" });
}

const originalGetBoundingClientRect = HTMLElement.prototype.getBoundingClientRect;
const originalGetAnimations = Object.getOwnPropertyDescriptor(
  Element.prototype,
  "getAnimations",
);
const originalResizeObserver = globalThis.ResizeObserver;
const resizeObservers: TestResizeObserver[] = [];

if (!originalGetAnimations) {
  Object.defineProperty(Element.prototype, "getAnimations", {
    configurable: true,
    value: () => [],
  });
}

function rect(width: number): DOMRect {
  return {
    bottom: 20,
    height: 20,
    left: 0,
    right: width,
    top: 0,
    width,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  } as DOMRect;
}

class TestResizeObserver implements ResizeObserver {
  constructor(private readonly callback: ResizeObserverCallback) {
    resizeObservers.push(this);
  }

  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}

  trigger(): void {
    this.callback([], this);
  }
}

globalThis.ResizeObserver = TestResizeObserver;
HTMLElement.prototype.getBoundingClientRect = function () {
  if (this.hasAttribute("data-permission-badges")) {
    return rect(Number(this.parentElement?.getAttribute("data-width") ?? 230));
  }
  if (
    this.hasAttribute("data-permission-measure") ||
    this.hasAttribute("data-overflow-measure")
  ) {
    return rect((this.textContent?.length ?? 0) * 7 + 16);
  }
  return originalGetBoundingClientRect.call(this);
};

const { act, cleanup, render, screen, waitFor, within } = await import(
  "@testing-library/react"
);
const userEventModule = await import("@testing-library/user-event");
const userEvent = userEventModule.default;
const { QueryClient, QueryClientProvider } = await import("@tanstack/react-query");
let ApiKeys = null as unknown as typeof import("./api-keys").ApiKeys;
let PermissionBadges = null as unknown as typeof import("./permission-badges").PermissionBadges;
const { PERMISSION_CATALOG } = await import("../lib/api");

const originalFetch = globalThis.fetch;
const fetchMock = vi.fn();

const rootPrincipal: Principal = {
  isRoot: true,
  name: "root",
  permissions: ["*"],
  role: "root",
};

function makeKey(overrides: Partial<ApiKey> = {}): ApiKey {
  return {
    createdAt: 1_700_000_000,
    id: 41,
    keyPrefix: "fixture-prefix",
    lastUsedAt: null,
    name: "studio-key",
    namespaces: ["*"],
    permissions: ["workers:read", "files:read"],
    role: "operator",
    workers: ["*"],
    ...overrides,
  };
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json" },
    status,
  });
}

function renderKeys(
  principal: Principal = rootPrincipal,
  locale: Locale = "en-US",
  renderPageAction?: (action: React.ReactNode) => React.ReactNode,
) {
  localStorage.setItem("edger.cpanel.locale", locale);
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
  });
  return render(
    <I18nProvider>
      <QueryClientProvider client={client}>
        <ApiKeys
          apiKey="test-editor-token"
          principal={principal}
          renderPageAction={renderPageAction}
        />
      </QueryClientProvider>
    </I18nProvider>,
  );
}

describe("PermissionBadges", () => {
  afterEach(() => {
    cleanup();
    resizeObservers.length = 0;
  });

  it("caps the badge container width independently of measured layout", () => {
    localStorage.setItem("edger.cpanel.locale", "en-US");
    const { container } = render(
      <I18nProvider>
        <PermissionBadges permissions={[...PERMISSION_CATALOG]} />
      </I18nProvider>,
    );
    const list = container.querySelector<HTMLElement>("[data-permission-badges]")!;

    expect(list.classList.contains("max-w-64")).toBe(true);
  });

  it("keeps badges within two measured lines and recalculates the hidden count on resize", async () => {
    const permissions = [...PERMISSION_CATALOG];
    localStorage.setItem("edger.cpanel.locale", "en-US");
    const { container } = render(
      <I18nProvider>
        <div data-testid="width-box" data-width="230">
          <PermissionBadges permissions={permissions} />
        </div>
      </I18nProvider>,
    );
    const box = screen.getByTestId("width-box");
    const list = container.querySelector<HTMLElement>("[data-permission-badges]")!;
    await waitFor(() =>
      expect(list.querySelector('[aria-label^="8 hidden permissions:"]')).toBeTruthy(),
    );
    const narrowItems = [...list.querySelectorAll<HTMLElement>("[role=listitem]")];
    expect(narrowItems).toHaveLength(4);
    expect(narrowItems.at(-1)?.getAttribute("aria-label")).toContain(
      "8 hidden permissions: workers:promote, workers:toggle",
    );
    expect(narrowItems.at(-1)?.title).toContain("observability:read, keys:manage");
    expect(new Set(narrowItems.map((item) => item.dataset.line)).size).toBeLessThanOrEqual(2);
    expect(list.getAttribute("aria-label")).toBe(
      `Permissions: ${permissions.join(", ")}`,
    );

    await act(async () => {
      box.setAttribute("data-width", "1500");
      resizeObservers.at(-1)?.trigger();
    });
    await waitFor(() =>
      expect(list.querySelectorAll("[role=listitem]")).toHaveLength(permissions.length),
    );
    expect(list.querySelector('[aria-label*="hidden permissions:"]')).toBeNull();
    expect(
      new Set(
        [...list.querySelectorAll<HTMLElement>("[role=listitem]")].map(
          (item) => item.dataset.line,
        ),
      ).size,
    ).toBeLessThanOrEqual(2);
  });
});

describe("ApiKeys permission editing", () => {
  afterEach(() => {
    cleanup();
    fetchMock.mockReset();
    globalThis.fetch = originalFetch;
    localStorage.clear();
    sessionStorage.clear();
  });

  it("renders a bounded sortable and paginated grid with the create action in the page actions slot", async () => {
    const longWorker = `worker-${"long-name-".repeat(12)}`;
    const permissions = [...PERMISSION_CATALOG];
    const keys = Array.from({ length: 16 }, (_, index) =>
      makeKey({
        id: index + 1,
        name: `key-${String(15 - index).padStart(2, "0")}`,
        permissions,
        workers: [longWorker],
      }),
    );
    fetchMock.mockResolvedValue(jsonResponse({ keys }));
    globalThis.fetch = fetchMock;

    renderKeys(
      rootPrincipal,
      "pt-BR",
      (action) => <div data-testid="page-actions-slot">{action}</div>,
    );
    const actionSlot = await screen.findByTestId("page-actions-slot");
    expect(
      within(actionSlot).getByRole("button", { name: "Nova chave" }),
    ).toBeTruthy();
    expect(screen.getAllByRole("button", { name: "Nova chave" })).toHaveLength(1);
    expect(await screen.findByText("Página 1 de 2")).toBeTruthy();

    const table = screen.getByRole("table");
    expect(within(table).getAllByRole("row")).toHaveLength(16);
    expect(within(table).getByText("key-15")).toBeTruthy();
    expect(within(table).queryByText("key-00")).toBeNull();
    expect(table.style.tableLayout).toBe("fixed");
    expect(table.style.minWidth).toBe("1184px");
    const permissionCell = table.querySelector("tbody tr td:nth-child(3)")!;
    expect((permissionCell as HTMLElement).style.width).toBe("256px");
    const permissionList = permissionCell.querySelector<HTMLElement>(
      "[data-permission-badges]",
    )!;
    const hiddenBadge = permissionList.querySelector<HTMLElement>(
      '[aria-label^="8 permissões ocultas:"]',
    );
    expect(hiddenBadge?.textContent).toBe("+8");
    expect(hiddenBadge?.getAttribute("aria-label")).toBe(
      `8 permissões ocultas: ${permissions.slice(3).join(", ")}`,
    );
    expect(permissionList.getAttribute("aria-label")).toBe(
      `Permissões: ${permissions.join(", ")}`,
    );
    const workerCell = table.querySelector<HTMLElement>(
      "tbody tr td:nth-child(4) code",
    );
    expect(workerCell?.classList.contains("truncate")).toBe(true);
    expect(workerCell?.title).toBe(longWorker);

    await userEvent.click(screen.getByRole("button", { name: "Nome" }));
    expect(within(table).getByText("key-00")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Próxima página" }));
    expect(within(table).getByText("key-15")).toBeTruthy();

    await userEvent.click(within(actionSlot).getByRole("button", { name: "Nova chave" }));
    expect(screen.getByRole("heading", { name: "Nova chave de API" })).toBeTruthy();
  });

  it.each([
    ["pt-BR", "Nova chave", "Nenhuma chave de API ainda."],
    ["en-US", "New key", "No API keys yet."],
    ["es-ES", "Nueva clave", "Aún no hay claves de API."],
  ] as const)("localizes the empty keys page for %s", async (locale, action, empty) => {
    fetchMock.mockResolvedValue(jsonResponse({ keys: [] }));
    globalThis.fetch = fetchMock;
    renderKeys(rootPrincipal, locale);

    expect(await screen.findByRole("button", { name: action })).toBeTruthy();
    expect(screen.getByText(empty, { exact: false })).toBeTruthy();
  });

  it("edits only permissions, keeps unauthorized existing permissions removable, and refetches the table", async () => {
    let active = makeKey();
    const revoked = makeKey({
      id: 42,
      name: "revoked-key",
      revokedAt: 1_710_000_000,
    });
    fetchMock.mockImplementation(async (_input: RequestInfo | URL, init?: RequestInit) => {
      if (init?.method === "PATCH") {
        active = { ...active, permissions: JSON.parse(String(init.body)).permissions };
        return jsonResponse(active);
      }
      return jsonResponse({ keys: [active, revoked] });
    });
    globalThis.fetch = fetchMock;

    renderKeys({
      isRoot: false,
      name: "limited-editor",
      permissions: ["keys:manage", "workers:read", "workers:invoke"],
      role: "operator",
    });
    const user = userEvent.setup();
    await screen.findByText("studio-key");
    expect(
      screen.queryByRole("button", { name: "Edit permissions for revoked-key" }),
    ).toBeNull();
    await user.click(
      screen.getByRole("button", { name: "Edit permissions for studio-key" }),
    );

    const dialog = screen.getByRole("dialog");
    const fields = within(dialog);
    expect((fields.getByRole("checkbox", { name: "workers:read" }) as HTMLInputElement).checked).toBe(true);
    const filesRead = fields.getByRole("checkbox", { name: "files:read" }) as HTMLInputElement;
    expect(filesRead.checked).toBe(true);
    expect(filesRead.disabled).toBe(false);
    expect((fields.getByRole("checkbox", { name: "workers:install" }) as HTMLInputElement).disabled).toBe(true);

    await user.click(filesRead);
    await user.click(fields.getByRole("checkbox", { name: "workers:invoke" }));
    await user.click(fields.getByRole("button", { name: "Save permissions" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const patchCall = fetchMock.mock.calls.find(([, init]) => init?.method === "PATCH");
    expect(patchCall).toBeTruthy();
    expect(patchCall?.[0]).toBe("http://localhost/api/admin/keys/41");
    expect(JSON.parse(String(patchCall?.[1]?.body))).toEqual({
      permissions: ["workers:read", "workers:invoke"],
    });
    await waitFor(() =>
      expect(
        screen.getAllByRole("list").some((list) =>
          within(list).queryByText("workers:invoke"),
        ),
      ).toBe(true),
    );
    expect(
      fetchMock.mock.calls.filter(([, init]) => init?.method !== "PATCH"),
    ).toHaveLength(2);
    expect(JSON.stringify(patchCall)).not.toContain("rawKey");
    expect(JSON.stringify(patchCall)).not.toContain("egk_");
  });

  it("keeps the edit dialog open on error and displays no secret from the response", async () => {
    const active = makeKey();
    fetchMock.mockImplementation(async (_input: RequestInfo | URL, init?: RequestInit) =>
      init?.method === "PATCH"
        ? jsonResponse({ message: "denied egk_SECRET_SHOULD_NOT_APPEAR" }, 403)
        : jsonResponse({ keys: [active] }),
    );
    globalThis.fetch = fetchMock;

    renderKeys();
    const user = userEvent.setup();
    await screen.findByText("studio-key");
    await user.click(
      screen.getByRole("button", { name: "Edit permissions for studio-key" }),
    );
    await user.click(screen.getByRole("button", { name: "Save permissions" }));

    expect(await screen.findByRole("alert").then((element) => element.textContent)).toContain(
      "Unable to update key permissions.",
    );
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(screen.queryByText(/egk_SECRET_SHOULD_NOT_APPEAR/)).toBeNull();
  });
});

beforeAll(async () => {
  if (!("happyDOM" in globalThis)) {
    GlobalRegistrator.register({ url: "http://localhost/cpanel/" });
  }
  const icons = () => {
    const icon = () => React.createElement("span", { "aria-hidden": true });
    return {
      ChevronDownIcon: icon,
      ChevronLeftIcon: icon,
      ChevronRightIcon: icon,
      ChevronUpIcon: icon,
      CheckIcon: icon,
      CopyIcon: icon,
      ChevronsLeftIcon: icon,
      ChevronsRightIcon: icon,
      ChevronsUpDown: icon,
      PencilIcon: icon,
      PlusIcon: icon,
      Trash2Icon: icon,
    };
  };
  if (bunTest) {
    bunTest.mock.module("@edger/ui/icons/lucide", icons);
    bunTest.mock.module(
      fileURLToPath(new URL("../../../../ui/src/icons/lucide.ts", import.meta.url)),
      icons,
    );
  }
  const modules = await Promise.all([
    import("./api-keys"),
    import("./permission-badges"),
  ]);
  ApiKeys = modules[0].ApiKeys;
  PermissionBadges = modules[1].PermissionBadges;
});

afterAll(() => {
  HTMLElement.prototype.getBoundingClientRect = originalGetBoundingClientRect;
  globalThis.ResizeObserver = originalResizeObserver;
  if (originalGetAnimations) {
    Object.defineProperty(Element.prototype, "getAnimations", originalGetAnimations);
  } else {
    Reflect.deleteProperty(Element.prototype, "getAnimations");
  }
});
