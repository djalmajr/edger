import { describe, expect, it } from "vitest";

import { compareSemver, kindLabel, workerUrl, type Worker } from "./api";

const worker: Worker = {
  kind: "fetch",
  name: "hello",
  namespace: "acme",
  status: "enabled",
  version: "1.2.3",
};

describe("cPanel API helpers", () => {
  it("formats versioned and latest worker paths", () => {
    expect(workerUrl(worker)).toBe("/@acme/hello@1.2.3");
    expect(workerUrl(worker, true)).toBe("/@acme/hello");
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
