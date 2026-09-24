import { describe, expect, it } from "vitest";

import {
  can,
  compareSemver,
  kindLabel,
  runtimeUrl,
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
