import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  adminLogout,
  ApiError,
  can,
  changeMyPassword,
  clearSession,
  compareSemver,
  createUser,
  deleteRoutingPolicy,
  deleteUser,
  getRoutingPolicy,
  isSessionToken,
  isValidUsername,
  kindLabel,
  listUsers,
  login,
  loginOptions,
  LOGOUT_TIMEOUT_MS,
  passwordPolicyIssues,
  putRoutingPolicy,
  resetUserPassword,
  routingPolicyPath,
  runtimeUrl,
  updateUser,
  workerUrl,
  type Worker,
} from "./api";

const worker: Worker = {
  kind: "fetch",
  name: "hello",
  namespace: "acme",
  status: "enabled",
  version: "1.2.3",
};

describe("cPanel API helpers", () => {
  it("formats versioned and latest worker paths from the runtime root", () => {
    // Resolved against the base href's parent — behind a stripping proxy
    // that is the proxied prefix, never a bare "/". In this DOM-less test
    // env the base falls back to http://localhost/.
    expect(workerUrl(worker)).toBe("http://localhost/@acme/hello@1.2.3");
    expect(workerUrl(worker, true)).toBe("http://localhost/@acme/hello");
  });

  it("resolves runtime paths without escaping the base", () => {
    expect(runtimeUrl("/api/admin/session")).toBe("http://localhost/api/admin/session");
    expect(runtimeUrl("metrics/stats")).toBe("http://localhost/metrics/stats");
  });

  it("normalizes worker kind values", () => {
    expect(kindLabel({ StaticSpa: {} })).toBe("StaticSpa");
    expect(kindLabel(null)).toBe("-");
  });

  it("orders semantic versions numerically", () => {
    expect(compareSemver("1.10.0", "1.2.9")).toBeGreaterThan(0);
    expect(compareSemver("2.0.0", "2.0.0")).toBe(0);
  });
});

describe("can", () => {
  const permissions = ["workers:read", "files:delete"];
  it("grants every permission to the root principal", () => {
    expect(can({ isRoot: true, permissions: [] }, "keys:manage")).toBe(true);
  });
  it("grants every permission to a wildcard principal", () => {
    expect(can({ permissions: ["*"] }, "files:write")).toBe(true);
  });
  it("grants a permission present in the list", () => {
    expect(can({ permissions }, "files:delete")).toBe(true);
  });
  it("denies a permission that is not present", () => {
    expect(can({ permissions }, "files:write")).toBe(false);
    expect(can({ permissions: [] }, "files:read")).toBe(false);
    expect(can({}, "files:read")).toBe(false);
  });
});

describe("routing policy client", () => {
  const originalFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  it("encodes the full app name in the query", () => {
    expect(routingPolicyPath("@scope/name")).toBe(
      "/api/admin/routing-policy?name=%40scope%2Fname",
    );
  });

  it("reads a null policy and a saved document", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json({ policy: null }),
    );
    globalThis.fetch = fetchMock;
    await expect(getRoutingPolicy("root-key", "@scope/name")).resolves.toBeNull();
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toContain("/api/admin/routing-policy?name=%40scope%2Fname");
    expect(new Headers(init.headers).get("x-api-key")).toBe("root-key");

    const policy = {
      name: "shop",
      tenantAccess: { mode: "allowlist" as const, tenants: ["acme"] },
      traffic: { versions: [{ version: "1.0.0", weight: 100 }] },
    };
    fetchMock.mockResolvedValueOnce(Response.json({ policy }));
    await expect(putRoutingPolicy("root-key", policy)).resolves.toEqual(policy);
    const put = fetchMock.mock.calls[1] as unknown as [string, RequestInit];
    expect(put[1].method).toBe("PUT");
    expect(JSON.parse(String(put[1].body))).toEqual(policy);

    fetchMock.mockResolvedValueOnce(Response.json({ deleted: true }));
    await expect(deleteRoutingPolicy("root-key", "shop")).resolves.toEqual({
      deleted: true,
    });
    expect(
      (fetchMock.mock.calls[2] as unknown as [string, RequestInit])[1].method,
    ).toBe(
      "DELETE",
    );
  });

  it("surfaces the server message and a missing delete flag", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json({ message: "workers:read required" }, { status: 403 }),
    );
    globalThis.fetch = fetchMock;
    await expect(getRoutingPolicy("root-key", "shop")).rejects.toThrow(
      "workers:read required",
    );
    fetchMock.mockResolvedValueOnce(Response.json({ deleted: false }));
    await expect(deleteRoutingPolicy("root-key", "shop")).rejects.toThrow(
      /missing deleted/,
    );
  });
});

describe("admin login client", () => {
  const originalFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  it("classifies session and legacy credentials", () => {
    expect(isSessionToken("ses-abc")).toBe(true);
    expect(isSessionToken("root-key")).toBe(false);
    expect(isSessionToken("egk_123")).toBe(false);
  });

  it("logs in with username/password and returns the session token", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json({ token: "ses-abc123" }),
    );
    globalThis.fetch = fetchMock;
    await expect(login("root", "s3cr3t!")).resolves.toBe("ses-abc123");
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toBe("http://localhost/api/admin/login");
    expect(init.method).toBe("POST");
    expect(
      new Headers(init.headers).get("content-type"),
    ).toBe("application/json");
    expect(JSON.parse(String(init.body))).toEqual({
      username: "root",
      password: "s3cr3t!",
    });
  });

  it("carries the HTTP status and never echoes credentials or server bodies", async () => {
    for (const status of [401, 429, 503]) {
      const fetchMock = vi.fn(async () =>
        Response.json({ message: "invalid credentials" }, { status }),
      );
      globalThis.fetch = fetchMock;
      const error = await login("root", "s3cr3t!").catch((reason) => reason);
      expect(error).toBeInstanceOf(ApiError);
      expect((error as ApiError).status).toBe(status);
      expect(error.message).not.toContain("s3cr3t!");
      expect(error.message).not.toContain("root");
      expect(error.message).not.toContain("invalid credentials");
    }
  });

  it("rejects malformed login success bodies whole", async () => {
    const fetchMock = vi.fn();
    globalThis.fetch = fetchMock;
    fetchMock.mockResolvedValueOnce(Response.json({}));
    await expect(login("root", "x")).rejects.toThrow(/missing token/);
    fetchMock.mockResolvedValueOnce(
      Response.json({ token: 123 }),
    );
    await expect(login("root", "x")).rejects.toThrow(/missing token/);
    fetchMock.mockResolvedValueOnce(
      new Response("<html>proxy error</html>", {
        headers: { "content-type": "text/html" },
      }),
    );
    await expect(login("root", "x")).rejects.toThrow(/missing token/);
  });

  it("reads login options without a credential and rejects odd shapes", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json({ passwordEnabled: true, rootSeeded: true }),
    );
    globalThis.fetch = fetchMock;
    await expect(loginOptions()).resolves.toEqual({
      passwordEnabled: true,
      rootSeeded: true,
    });
    expect(
      new Headers(
        (fetchMock.mock.calls[0] as unknown as [string, RequestInit?])[1]
          ?.headers ?? undefined,
      ).get("x-api-key"),
    ).toBeNull();
    expect(
      (fetchMock.mock.calls[0] as unknown as [string])[0],
    ).toBe("http://localhost/api/admin/login-options");

    fetchMock
      .mockResolvedValueOnce(
        Response.json({ passwordEnabled: "yes", rootSeeded: true }),
      )
      .mockResolvedValueOnce(Response.json({}))
      .mockResolvedValueOnce(
        new Response("not json", { headers: { "content-type": "text/plain" } }),
      )
      .mockResolvedValueOnce(
        Response.json({ passwordEnabled: true, rootSeeded: true }, { status: 404 }),
      )
      .mockRejectedValueOnce(new TypeError("fetch failed"));
    for (let attempt = 0; attempt < 5; attempt += 1) {
      await expect(loginOptions()).resolves.toBeNull();
    }
  });

  it("posts logout with the session token and surfaces failures as ApiError", async () => {
    const fetchMock = vi.fn(async () =>
      new Response(null, { status: 204 }),
    );
    globalThis.fetch = fetchMock;
    await expect(adminLogout("ses-abc")).resolves.toBeUndefined();
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toBe("http://localhost/api/admin/logout");
    expect(init.method).toBe("POST");
    expect(new Headers(init.headers).get("x-api-key")).toBe("ses-abc");

    fetchMock.mockResolvedValueOnce(
      Response.json({ message: "not found" }, { status: 401 }),
    );
    const error = await adminLogout("ses-gone").catch((reason) => reason);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(401);
  });
});

describe("clearSession", () => {
  const originalFetch = globalThis.fetch;
  let originalSessionStorage: PropertyDescriptor | undefined;

  function createSessionStorage(): Storage {
    const entries = new Map<string, string>();
    return {
      get length() {
        return entries.size;
      },
      clear: () => entries.clear(),
      getItem: (key) => entries.get(String(key)) ?? null,
      key: (index) => Array.from(entries.keys())[index] ?? null,
      removeItem: (key) => entries.delete(String(key)),
      setItem: (key, value) => entries.set(String(key), String(value)),
    };
  }

  beforeEach(() => {
    originalSessionStorage = Object.getOwnPropertyDescriptor(
      globalThis,
      "sessionStorage",
    );
    Object.defineProperty(globalThis, "sessionStorage", {
      configurable: true,
      value: createSessionStorage(),
      writable: true,
    });
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
    if (originalSessionStorage) {
      Object.defineProperty(
        globalThis,
        "sessionStorage",
        originalSessionStorage,
      );
    } else {
      Reflect.deleteProperty(globalThis, "sessionStorage");
    }
  });

  it("clears the local session before any network answer and still revokes when the network works", async () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
    globalThis.fetch = fetchMock;
    sessionStorage.setItem("edger.cpanel.apiKey", "ses-abc");
    const done = clearSession("ses-abc");
    // Local cleanup is immediate, before the endpoint answers.
    expect(sessionStorage.getItem("edger.cpanel.apiKey")).toBeNull();
    await done;
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toBe("http://localhost/api/admin/logout");
    expect(new Headers(init.headers).get("x-api-key")).toBe("ses-abc");
  });

  it("ends the logout and aborts the attempt when the network never answers", async () => {
    const started = Date.now();
    const fetchMock = vi.fn(async () => {
      // Never settles: a logout that never answers.
      return new Promise<Response>(() => undefined);
    });
    globalThis.fetch = fetchMock;
    sessionStorage.setItem("edger.cpanel.apiKey", "ses-abc");
    const done = clearSession("ses-abc");
    // Local cleanup is immediate, before any network answer.
    expect(sessionStorage.getItem("edger.cpanel.apiKey")).toBeNull();
    await done;
    // The attempt ended at the bounded timeout, not when the fetch answered
    // (it never did): a slow network must never hold the signed-out user.
    expect(Date.now() - started).toBeLessThan(2 * LOGOUT_TIMEOUT_MS);
    const [, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect((init.signal as AbortSignal | undefined)?.aborted).toBe(true);
  });

  it("clears non-session credentials locally without calling the endpoint", async () => {
    const fetchMock = vi.fn();
    globalThis.fetch = fetchMock;
    sessionStorage.setItem("edger.cpanel.apiKey", "root-key");
    await clearSession("root-key");
    expect(sessionStorage.getItem("edger.cpanel.apiKey")).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("console user management client", () => {
  const originalFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  const rootUser = {
    createdAt: 1700000000,
    disabled: false,
    id: 1,
    isRoot: true,
    namespaces: ["*"],
    permissions: ["*"],
    role: "root",
    username: "root",
    workers: ["*"],
  };
  const extraUser = {
    createdAt: 1700000001,
    disabled: false,
    id: 2,
    isRoot: false,
    namespaces: ["acme"],
    permissions: ["workers:read"],
    role: "operator",
    username: "analyst-01",
    workers: ["p-abc*"],
  };

  it("lists users with the session credential and validates the whole body", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json({ users: [rootUser, extraUser] }),
    );
    globalThis.fetch = fetchMock;
    await expect(listUsers("ses-root")).resolves.toEqual([rootUser, extraUser]);
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toBe("http://localhost/api/admin/users");
    // No explicit method: the fetch defaults to GET.
    expect(init.method ?? "GET").toBe("GET");
    expect(new Headers(init.headers).get("x-api-key")).toBe("ses-root");

    fetchMock.mockResolvedValueOnce(Response.json({ users: [rootUser, { id: 3 }] }));
    await expect(listUsers("ses-root")).rejects.toThrow(/malformed/);
    fetchMock.mockResolvedValueOnce(Response.json({ bogus: true }));
    await expect(listUsers("ses-root")).rejects.toThrow(/malformed/);
  });

  it("creates a user and reads the metadata back, never the password", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json({ user: extraUser }, { status: 201 }),
    );
    globalThis.fetch = fetchMock;
    const created = await createUser("ses-root", {
      namespaces: ["acme"],
      password: "str0ng!passw0rd",
      permissions: ["workers:read"],
      username: "analyst-01",
      workers: ["p-abc*"],
    });
    expect(created).toEqual(extraUser);
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toBe("http://localhost/api/admin/users");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      namespaces: ["acme"],
      password: "str0ng!passw0rd",
      permissions: ["workers:read"],
      username: "analyst-01",
      workers: ["p-abc*"],
    });
    // A 409 on the duplicate username surfaces as an ApiError.
    fetchMock.mockResolvedValueOnce(
      Response.json({ code: "USERNAME_TAKEN", message: "username already exists" }, { status: 409 }),
    );
    const error = await createUser("ses-root", {
      namespaces: ["*"],
      password: "str0ng!passw0rd",
      permissions: ["workers:read"],
      username: "analyst-01",
      workers: ["*"],
    }).catch((reason) => reason);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(409);
  });

  it("patches grants and status of a user by id", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json({ user: { ...extraUser, disabled: true } }),
    );
    globalThis.fetch = fetchMock;
    await expect(updateUser("ses-root", 2, { disabled: true })).resolves.toEqual({
      ...extraUser,
      disabled: true,
    });
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toBe("http://localhost/api/admin/users/2");
    expect(init.method).toBe("PATCH");
    expect(JSON.parse(String(init.body))).toEqual({ disabled: true });
  });

  it("resets a password and deletes a user without keeping the secret", async () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
    globalThis.fetch = fetchMock;
    await expect(resetUserPassword("ses-root", 2, "an0ther!strong")).resolves.toBeUndefined();
    const [resetUrl, resetInit] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(resetUrl).toBe("http://localhost/api/admin/users/2/reset-password");
    expect(resetInit.method).toBe("POST");
    expect(JSON.parse(String(resetInit.body))).toEqual({
      password: "an0ther!strong",
    });

    await expect(deleteUser("ses-root", 2)).resolves.toBeUndefined();
    const [deleteUrl, deleteInit] = fetchMock.mock.calls[1] as unknown as [
      string,
      RequestInit,
    ];
    expect(deleteUrl).toBe("http://localhost/api/admin/users/2");
    expect(deleteInit.method).toBe("DELETE");
  });

  it("changes the own password and distinguishes a rotated token from a re-login", async () => {
    const fetchMock = vi.fn(async () =>
      Response.json({ token: "ses-new" }),
    );
    globalThis.fetch = fetchMock;
    await expect(
      changeMyPassword("ses-old", "old!Passw0rd123", "new!Passw0rd123"),
    ).resolves.toBe("ses-new");
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(url).toBe("http://localhost/api/admin/me/password");
    expect(init.method).toBe("POST");
    expect(new Headers(init.headers).get("x-api-key")).toBe("ses-old");
    expect(JSON.parse(String(init.body))).toEqual({
      current: "old!Passw0rd123",
      new: "new!Passw0rd123",
    });

    // No token in the answer: the runtime requires a fresh login.
    fetchMock
      .mockResolvedValueOnce(Response.json({}))
      .mockResolvedValueOnce(
        new Response(null, { status: 200 }),
      );
    for (let attempt = 0; attempt < 2; attempt += 1) {
      await expect(
        changeMyPassword("ses-old", "a", "b"),
      ).resolves.toBeNull();
    }

    // Failure never echoes credentials or the server body.
    fetchMock.mockResolvedValueOnce(
      Response.json({ message: "invalid credentials" }, { status: 401 }),
    );
    const error = await changeMyPassword(
      "ses-old",
      "old!Passw0rd123",
      "new!Passw0rd123",
    ).catch((reason) => reason);
    expect(error).toBeInstanceOf(ApiError);
    expect((error as ApiError).status).toBe(401);
    expect(error.message).not.toContain("old!Passw0rd123");
    expect(error.message).not.toContain("new!Passw0rd123");
    expect(error.message).not.toContain("invalid credentials");
  });
});

describe("console credential policy mirrors", () => {
  it("flags the strong-password policy gaps", () => {
    expect(passwordPolicyIssues("str0ng!passw0rd")).toEqual([]);
    expect(passwordPolicyIssues("short!1")).toContain("length");
    expect(passwordPolicyIssues("x".repeat(129))).toContain("length");
    expect(passwordPolicyIssues("0123456789.1234")).toContain("letter");
    expect(passwordPolicyIssues("abcdefghijklm.")).toContain("digit");
    expect(passwordPolicyIssues("abcdefghijkl12")).toContain("symbol");
    // 12 and 128 characters are the inclusive bounds.
    expect(passwordPolicyIssues("a".repeat(10) + "1!")).toEqual([]);
    expect(passwordPolicyIssues("a".repeat(126) + "1!")).toEqual([]);
  });

  it("validates the normalized username rule", () => {
    expect(isValidUsername("analyst-01")).toBe(true);
    expect(isValidUsername("a.b_c-d9")).toBe(true);
    expect(isValidUsername("a")).toBe(false);
    expect(isValidUsername(".".concat("x".repeat(31)))).toBe(false);
    expect(isValidUsername(".leading-dot")).toBe(false);
    expect(isValidUsername("-leading-hyphen")).toBe(false);
    expect(isValidUsername("Upper")).toBe(false);
    expect(isValidUsername("space name")).toBe(false);
    expect(isValidUsername("".concat("x".repeat(33)))).toBe(false);
  });
});
