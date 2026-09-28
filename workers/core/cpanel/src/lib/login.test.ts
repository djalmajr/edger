import { ensureLoginDom } from "./login.dom";
import { beforeAll, afterEach, describe, expect, it, vi } from "vitest";

// The DOM must exist before ./api evaluates: it freezes the runtime root from
// document.baseURI at module load.
const {
  clearSession,
  isSessionToken,
  runtimeUrl,
  SESSION_KEY,
} = await import("./api");

const runtimeRoot = new URL("..", document.baseURI).href;

describe("login routes follow the injected base", () => {
  beforeAll(() => {
    // bun evaluates all test files' modules before any test runs; a sibling
    // file's afterAll may have dropped the shared DOM globals in between.
    ensureLoginDom();
  });
  it("resolves admin login paths against the base parent, not the SPA prefix", () => {
    for (const path of [
      "/api/admin/login",
      "/api/admin/login-options",
      "/api/admin/logout",
    ]) {
      expect(runtimeUrl(path)).toBe(
        new URL(path.replace(/^\/+/, ""), runtimeRoot).href,
      );
      expect(runtimeUrl(path)).not.toContain("/apps/cpanel/api");
      expect(runtimeUrl(path)).not.toContain("/cpanel/api");
    }
  });
});

describe("clearSession", () => {
  const originalFetch = globalThis.fetch;

  afterEach(() => {
    globalThis.fetch = originalFetch;
    sessionStorage.clear();
  });

  // happy-dom's Storage ignores own-property stubs of removeItem, so the
  // effect ORDER is proven by reading the store inside the fetch: the local
  // clear happens FIRST, so the store must already be empty while the
  // revocation POST is in flight, and the POST still carries the token.
  it("clears the local session first, then revokes the ses- session", async () => {
    sessionStorage.setItem(SESSION_KEY, "ses-abc");
    let itemAtFetchTime: string | null = "unset";
    let headerAtFetchTime: string | null = null;
    globalThis.fetch = vi.fn(async (_url: unknown, init?: RequestInit) => {
      itemAtFetchTime = sessionStorage.getItem(SESSION_KEY);
      headerAtFetchTime =
        init && "headers" in init
          ? new Headers(init.headers).get("x-api-key")
          : null;
      return new Response(null, {
        status: init?.method === "POST" ? 204 : 405,
      });
    });
    await clearSession("ses-abc");
    // The store was already empty while the revocation POST was in flight.
    expect(itemAtFetchTime).toBeNull();
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    // The remote revocation still happened, with the session credential.
    expect(headerAtFetchTime).toBe("ses-abc");
  });

  it("clears locally even when revocation fails", async () => {
    sessionStorage.setItem(SESSION_KEY, "ses-gone");
    let itemAtFetchTime: string | null = "unset";
    let attempts = 0;
    globalThis.fetch = vi.fn(async () => {
      itemAtFetchTime = sessionStorage.getItem(SESSION_KEY);
      attempts += 1;
      return Response.json({ message: "not found" }, { status: 500 });
    });
    await expect(clearSession("ses-gone")).resolves.toBeUndefined();
    // The local clear did not wait for the failing revocation, and the
    // attempt was still made.
    expect(itemAtFetchTime).toBeNull();
    expect(attempts).toBe(1);
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
  });

  it("clears the local session immediately even when the logout never answers", async () => {
    sessionStorage.setItem(SESSION_KEY, "ses-pending");
    globalThis.fetch = vi.fn(
      () => new Promise<Response>(() => undefined),
    );
    const done = clearSession("ses-pending");
    // The navigation to login must not wait for the network: the store is
    // empty before the (never-answering) revocation settles.
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    // The logout still ends: at the bounded timeout, not when the fetch
    // answers (it never does).
    await done;
  });

  it("only clears locally for non-session credentials", async () => {
    sessionStorage.setItem(SESSION_KEY, "root-key");
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
    globalThis.fetch = fetchMock;
    await clearSession("root-key");
    expect(fetchMock).not.toHaveBeenCalled();
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    expect(isSessionToken("root-key")).toBe(false);
  });
});
