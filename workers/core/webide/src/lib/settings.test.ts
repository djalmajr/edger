import { describe, expect, it } from "vitest";

import {
  DEFAULT_SETTINGS,
  filterSettingDefinitions,
  getSettingValueForScope,
  isPathExcluded,
  isSettingModified,
  resolveSettings,
  sanitizeSettings,
  SETTINGS_DEFINITIONS,
  unsetPartialSetting,
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

  it("sanitizes the expanded editor, layout, preview, and autosave settings", () => {
    expect(
      sanitizeSettings({
        editor: { lineNumbers: false },
        files: { autoSaveDelay: 750 },
        preview: { autoPreview: false },
        workbench: { panelVisible: false, previewVisible: false },
      }),
    ).toEqual({
      editor: { lineNumbers: false },
      files: { autoSaveDelay: 750 },
      preview: { autoPreview: false },
      workbench: { panelVisible: false, previewVisible: false },
    });

    expect(sanitizeSettings({ files: { autoSaveDelay: 99 } })).toEqual({});
    expect(sanitizeSettings({ files: { autoSaveDelay: 5001 } })).toEqual({});
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

  it("removes an override and prunes its empty group", () => {
    expect(
      unsetPartialSetting(
        { editor: { fontSize: 16 }, preview: { autoPreview: false } },
        "editor",
        "fontSize",
      ),
    ).toEqual({ preview: { autoPreview: false } });
  });

  it("reads the editable value without leaking workspace overrides into user", () => {
    const snapshot = {
      user: { editor: { fontSize: 16 } },
      workspace: { editor: { fontSize: 13 } },
      resolved: {
        settings: resolveSettings(
          { editor: { fontSize: 16 } },
          { editor: { fontSize: 13 } },
        ),
      },
      hasWorkspace: true,
    };

    expect(
      getSettingValueForScope(snapshot, "user", "editor", "fontSize"),
    ).toBe(16);
    expect(
      getSettingValueForScope(snapshot, "workspace", "editor", "fontSize"),
    ).toBe(13);
  });

  it("inherits user and default values when the selected scope has no override", () => {
    const snapshot = {
      user: { editor: { fontSize: 16 } },
      workspace: {},
      resolved: {
        settings: resolveSettings({ editor: { fontSize: 16 } }, {}),
      },
      hasWorkspace: true,
    };

    expect(
      getSettingValueForScope(snapshot, "workspace", "editor", "fontSize"),
    ).toBe(16);
    expect(
      getSettingValueForScope(snapshot, "user", "editor", "tabSize"),
    ).toBe(DEFAULT_SETTINGS.editor.tabSize);
  });

  it("keeps theme user-scoped even when legacy workspace data exists", () => {
    expect(
      resolveSettings(
        { workbench: { theme: "light" } },
        { workbench: { theme: "dark" } },
      ).workbench.theme,
    ).toBe("light");
    expect(
      SETTINGS_DEFINITIONS.find(
        (definition) => definition.id === "workbench.theme",
      )?.scopes,
    ).toEqual(["user"]);
  });

  it("filters settings by metadata and modified state", () => {
    const snapshot = {
      user: { editor: { wordWrap: true } },
      workspace: {},
      resolved: {
        settings: resolveSettings({ editor: { wordWrap: true } }, {}),
      },
      hasWorkspace: true,
    };

    expect(
      filterSettingDefinitions(SETTINGS_DEFINITIONS, {
        query: "long lines",
        scope: "user",
        snapshot,
      }).map((definition) => definition.id),
    ).toContain("editor.wordWrap");
    expect(
      filterSettingDefinitions(SETTINGS_DEFINITIONS, {
        modifiedOnly: true,
        query: "",
        scope: "user",
        snapshot,
      }).map((definition) => definition.id),
    ).toEqual(["editor.wordWrap"]);
    expect(
      isSettingModified(snapshot, "workspace", "editor", "wordWrap"),
    ).toBe(false);
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
