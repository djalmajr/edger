import { describe, expect, it } from "vitest";

import {
  cpanelBasePath,
  readRoute,
  routePath,
  type RouteState,
} from "./route";

const target = { name: "@team/app", version: "1.2.3" };

const routes: Record<string, RouteState> = {
  overview: { path: "", view: "overview" },
  workers: { path: "", view: "workers" },
  keys: { path: "", view: "keys" },
  observability: { path: "", view: "observability" },
  logs: { path: "", view: "logs" },
  "worker files": { path: "src/hello world/índex.txt", target, view: "files" },
  "worker logs": { path: "", target, view: "logs" },
  "worker observability": { path: "", target, view: "observability" },
};

function roundTrip(base: string) {
  for (const route of Object.values(routes)) {
    expect(readRoute(routePath(route, base), base)).toEqual(route);
  }
}

for (const base of ["/apps/cpanel", "/cpanel"]) {
  describe(`basePath ${base}`, () => {
    it("round-trips every view through routePath/readRoute", () => {
      roundTrip(base);
    });

    it("maps the base path and its trailing slash to overview", () => {
      expect(readRoute(base, base)).toEqual({ path: "", view: "overview" });
      expect(readRoute(`${base}/`, base)).toEqual({
        path: "",
        view: "overview",
      });
    });

    it("returns overview for paths outside the base", () => {
      const outside =
        base === "/apps/cpanel" ? "/cpanel/keys" : "/apps/cpanel/keys";
      expect(readRoute(outside, base)).toEqual({ path: "", view: "overview" });
    });
  });
}

describe("explicit prefix examples", () => {
  it("parses a proxied path under the proxied base", () => {
    expect(readRoute("/apps/cpanel/keys", "/apps/cpanel")).toEqual({
      path: "",
      view: "keys",
    });
  });

  it("ignores the bare prefix when mounted proxied", () => {
    expect(readRoute("/cpanel/keys", "/apps/cpanel")).toEqual({
      path: "",
      view: "overview",
    });
  });

  it("parses a bare path under the bare base", () => {
    expect(readRoute("/cpanel/keys", "/cpanel")).toEqual({
      path: "",
      view: "keys",
    });
  });
});

describe("versioned basePath /cpanel@0.3.1", () => {
  const base = "/cpanel@0.3.1";

  it("round-trips every view", () => {
    roundTrip(base);
  });

  it("parses top-level views under the versioned prefix", () => {
    expect(readRoute(`${base}/keys`, base)).toEqual({ path: "", view: "keys" });
    expect(readRoute(`${base}/workers`, base)).toEqual({
      path: "",
      view: "workers",
    });
    expect(readRoute(`/cpanel/keys`, base)).toEqual({
      path: "",
      view: "overview",
    });
  });
});

describe("cpanelBasePath", () => {
  it("keeps the worker base path when present", () => {
    expect(cpanelBasePath("/apps/cpanel")).toBe("/apps/cpanel");
  });

  it("falls back to /cpanel when empty", () => {
    expect(cpanelBasePath("")).toBe("/cpanel");
  });
});
