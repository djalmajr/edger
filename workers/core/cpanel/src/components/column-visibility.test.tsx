import { GlobalRegistrator } from "@happy-dom/global-registrator";
import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
// Registers the shared bun-only union mocks (icon barrel + dropdown-menu)
// before any consumer loads; inert under the vite-backed runner.
import "./bun-ui-mocks";

// Imported after the bun mocks are registered: a static import would resolve
// the @edger/ui barrel (and its virtual ~icons modules) before the stubs
// exist — the same reason the sibling fixture files import dynamically.
let helpers: typeof import("./column-visibility");

// The column ids passed by the screens never include the actions column: it
// is not hideable, so a stored actions flag must be dropped on load.
const COLUMN_IDS = ["name", "permissions", "workers"] as const;
const KNOWN = new Set<string>(COLUMN_IDS);
const FALLBACK = { permissions: false };

beforeAll(async () => {
  if (!("happyDOM" in globalThis)) {
    GlobalRegistrator.register({ url: "http://localhost/cpanel/" });
  }
  helpers = await import("./column-visibility");
});

const KEYS_STORAGE = () => helpers.columnVisibilityStorageKey("keys");
const USERS_STORAGE = () => helpers.columnVisibilityStorageKey("users");

afterEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

// Shadow the fixture window's localStorage getter with one that throws
// (blocked storage, private contexts) and restore the previous descriptor in
// finally. The read/write helpers access window.localStorage inside their
// try, so a throwing getter must look like "storage unavailable", never like
// a page error.
function withBlockedLocalStorageGetter<T>(run: () => T): T {
  const previous = Object.getOwnPropertyDescriptor(window, "localStorage");
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    get() {
      throw new Error("SecurityError: localStorage access blocked");
    },
  });
  try {
    return run();
  } finally {
    if (previous) {
      Object.defineProperty(window, "localStorage", previous);
    } else {
      delete (window as { localStorage?: unknown }).localStorage;
    }
  }
}

describe("sanitizeVisibility", () => {
  it("keeps only known column ids with boolean flags", () => {
    expect(
      helpers.sanitizeVisibility(
        { permissions: true, workers: "yes", actions: false, bogus: 1 },
        KNOWN,
      ),
    ).toEqual({ permissions: true });
  });

  it("rejects non-object payloads", () => {
    expect(helpers.sanitizeVisibility(null, KNOWN)).toEqual({});
    expect(helpers.sanitizeVisibility("true", KNOWN)).toEqual({});
    expect(helpers.sanitizeVisibility(true, KNOWN)).toEqual({});
    expect(helpers.sanitizeVisibility([true], KNOWN)).toEqual({});
  });
});

describe("stored visibility (isolated read/write)", () => {
  it("round-trips a sanitized preference", () => {
    const key = KEYS_STORAGE();
    helpers.writeStoredVisibility(key, {
      permissions: true,
      workers: false,
    });
    expect(helpers.readStoredVisibility(key, KNOWN, FALLBACK)).toEqual({
      permissions: true,
      workers: false,
    });
  });

  it("falls back to the defaults when nothing is stored", () => {
    expect(helpers.readStoredVisibility(KEYS_STORAGE(), KNOWN, FALLBACK)).toEqual(
      FALLBACK,
    );
  });

  it("keeps the defaults for an invalid payload or unknown ids only", () => {
    const key = KEYS_STORAGE();
    localStorage.setItem(key, "{not-json");
    expect(helpers.readStoredVisibility(key, KNOWN, FALLBACK)).toEqual(
      FALLBACK,
    );
    localStorage.setItem(
      key,
      JSON.stringify({ bogus: false, permissions: true }),
    );
    expect(helpers.readStoredVisibility(key, KNOWN, FALLBACK)).toEqual({
      permissions: true,
    });
  });

  it("keeps the page working when the localStorage getter itself throws", () => {
    const key = KEYS_STORAGE();
    expect(
      withBlockedLocalStorageGetter(() =>
        helpers.readStoredVisibility(key, KNOWN, FALLBACK),
      ),
    ).toEqual(FALLBACK);
    expect(
      withBlockedLocalStorageGetter(() =>
        helpers.writeStoredVisibility(key, { permissions: true }),
      ),
    ).toBeUndefined();
  });
});

describe("useColumnVisibility", () => {
  function renderVisibility(storageKey: string) {
    return renderHook(() =>
      helpers.useColumnVisibility({
        columnIds: COLUMN_IDS,
        defaultVisibility: FALLBACK,
        storageKey,
      }),
    );
  }

  it("falls back to the defaults when nothing is stored", () => {
    const { result } = renderVisibility(KEYS_STORAGE());
    expect(result.current.columnVisibility).toEqual(FALLBACK);
  });

  it("restores the stored preference on mount (reload)", () => {
    const key = KEYS_STORAGE();
    localStorage.setItem(
      key,
      JSON.stringify({ permissions: true, workers: false }),
    );
    const { result } = renderVisibility(key);
    expect(result.current.columnVisibility).toEqual({
      permissions: true,
      workers: false,
    });
  });

  it("drops a stored actions flag so the actions column stays visible", () => {
    const key = KEYS_STORAGE();
    localStorage.setItem(
      key,
      JSON.stringify({ actions: false, permissions: true }),
    );
    const { result } = renderVisibility(key);
    expect(result.current.columnVisibility).toEqual({ permissions: true });
  });

  it("persists toggles and keeps the keys/users tables isolated", () => {
    const { result } = renderVisibility(KEYS_STORAGE());
    act(() => {
      result.current.onColumnVisibilityChange((previous) => ({
        ...previous,
        permissions: true,
      }));
    });
    expect(result.current.columnVisibility).toEqual({ permissions: true });
    expect(JSON.parse(localStorage.getItem(KEYS_STORAGE())!)).toEqual({
      permissions: true,
    });
    const { result: users } = renderVisibility(USERS_STORAGE());
    expect(users.current.columnVisibility).toEqual(FALLBACK);
  });

  it("keeps the state working when storage reads or writes fail", () => {
    vi.spyOn(localStorage, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    const { result } = renderVisibility(KEYS_STORAGE());
    expect(result.current.columnVisibility).toEqual(FALLBACK);
    vi.spyOn(localStorage, "setItem").mockImplementation(() => {
      throw new Error("quota exceeded");
    });
    act(() => {
      result.current.onColumnVisibilityChange((previous) => ({
        ...previous,
        permissions: true,
      }));
    });
    expect(result.current.columnVisibility).toEqual({ permissions: true });
  });
});
