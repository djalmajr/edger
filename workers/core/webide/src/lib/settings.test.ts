import { describe, expect, it } from "vitest";

import {
  DEFAULT_SETTINGS,
  isPathExcluded,
  resolveSettings,
  sanitizeSettings,
} from "./settings";

describe("settings model", () => {
  it("sanitizes invalid values and deduplicates excludes", () => {
    expect(
      sanitizeSettings({
        editor: { fontSize: 99, tabSize: 4 },
        files: { exclude: ["node_modules/**", "node_modules/**", ""] },
      }),
    ).toEqual({
      editor: { tabSize: 4 },
      files: { exclude: ["node_modules/**"] },
    });
  });

  it("lets workspace settings override user settings", () => {
    expect(
      resolveSettings(
        { editor: { fontSize: 16 }, preview: { autoPreview: false } },
        { editor: { fontSize: 13 } },
      ),
    ).toMatchObject({
      editor: { ...DEFAULT_SETTINGS.editor, fontSize: 13 },
      preview: { autoPreview: false },
    });
  });

  it("matches basename and nested glob exclusions", () => {
    expect(isPathExcluded("src/generated/client.ts", ["**/generated/**"])).toBe(
      true,
    );
    expect(
      isPathExcluded("node_modules/react/index.js", ["node_modules/**"]),
    ).toBe(true);
    expect(isPathExcluded("src/main.ts", ["node_modules/**"])).toBe(false);
  });
});
