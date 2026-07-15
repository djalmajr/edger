import { describe, expect, it } from "vitest";

import {
  createProject,
  parseManifest,
  slugify,
  validateFilePath,
  validateProject,
} from "./projects";

describe("project model", () => {
  it("creates a deployable project from a supported template", () => {
    const project = createProject("FetchHandler", "Hello EdgeR");

    expect(project.name).toBe("hello-edger");
    expect(validateProject(project)).toMatchObject({
      entrypoint: "index.ts",
      name: "hello-edger",
      version: "1.0.0",
    });
  });

  it("parses quoted manifest values", () => {
    expect(
      parseManifest('name: app\nversion: "2.1.0"\nentrypoint: index.ts\n'),
    ).toEqual({
      entrypoint: "index.ts",
      kind: undefined,
      name: "app",
      version: "2.1.0",
    });
  });

  it("normalizes names and rejects traversal paths", () => {
    expect(slugify("  Minha Aplicação  ")).toBe("minha-aplica-o");
    expect(() => validateFilePath("../secret.txt")).toThrow(
      "Invalid project file path",
    );
  });
});
