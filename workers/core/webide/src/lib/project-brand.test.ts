import { describe, expect, it } from "vitest";

import { detectProjectBrand } from "./project-brand";
import type { ProjectType } from "./projects";

function project(type: ProjectType, files: Record<string, string>) {
  return { files, type };
}

describe("detectProjectBrand", () => {
  it("prefers the framework over its build tool", () => {
    expect(
      detectProjectBrand(
        project("StaticSpa", {
          "package.json": JSON.stringify({
            dependencies: { react: "19.0.0" },
            devDependencies: { vite: "7.0.0" },
          }),
        }),
      ),
    ).toBe("react");
  });

  it("recognizes vanilla Vite projects", () => {
    expect(
      detectProjectBrand(
        project("StaticSpa", {
          "package.json": JSON.stringify({ devDependencies: { vite: "7" } }),
          "vite.config.js": "export default {}",
        }),
      ),
    ).toBe("vite");
  });

  it("uses Node for generic package projects", () => {
    expect(
      detectProjectBrand(
        project("FetchHandler", {
          "package.json": JSON.stringify({ dependencies: { express: "5" } }),
        }),
      ),
    ).toBe("node");
  });

  it("falls back to the EdgeR execution model", () => {
    expect(detectProjectBrand(project("FetchHandler", {}))).toBe("deno");
    expect(detectProjectBrand(project("StaticSpa", {}))).toBe("html");
  });
});
