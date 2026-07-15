import { normalizeTheme, type ThemePreference } from "@edger/ui/lib/theme";

export type FullSettings = {
  editor: {
    fontFamily: string;
    fontSize: number;
    tabSize: number;
    wordWrap: boolean;
  };
  files: { exclude: string[] };
  logs: { preserveAcrossRestarts: boolean };
  preview: { autoPreview: boolean };
  workbench: { theme: ThemePreference };
};
export type PartialSettings = {
  [Group in keyof FullSettings]?: Partial<FullSettings[Group]>;
};
export type SettingsScope = "user" | "workspace";
export type SettingsSnapshot = {
  user: PartialSettings;
  workspace: PartialSettings;
  resolved: { settings: FullSettings };
  hasWorkspace: boolean;
};

export const USER_SETTINGS_KEY = "edger.webide.userSettings";
export const DEFAULT_SETTINGS: FullSettings = {
  editor: {
    fontFamily: '"SFMono-Regular", Consolas, monospace',
    fontSize: 14,
    tabSize: 2,
    wordWrap: false,
  },
  files: { exclude: [] },
  logs: { preserveAcrossRestarts: false },
  preview: { autoPreview: true },
  workbench: { theme: "system" },
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function sanitizeSettings(input: unknown): PartialSettings {
  if (!isRecord(input)) return {};
  const output: PartialSettings = {};
  if (isRecord(input.editor)) {
    const editor: Partial<FullSettings["editor"]> = {};
    if (
      typeof input.editor.fontFamily === "string" &&
      input.editor.fontFamily.trim()
    )
      editor.fontFamily = input.editor.fontFamily.trim();
    if (
      typeof input.editor.fontSize === "number" &&
      input.editor.fontSize >= 8 &&
      input.editor.fontSize <= 32
    )
      editor.fontSize = input.editor.fontSize;
    if (
      typeof input.editor.tabSize === "number" &&
      input.editor.tabSize >= 1 &&
      input.editor.tabSize <= 8
    )
      editor.tabSize = input.editor.tabSize;
    if (typeof input.editor.wordWrap === "boolean")
      editor.wordWrap = input.editor.wordWrap;
    if (Object.keys(editor).length) output.editor = editor;
  }
  if (isRecord(input.files) && Array.isArray(input.files.exclude))
    output.files = {
      exclude: [
        ...new Set(
          input.files.exclude
            .filter((value): value is string => typeof value === "string")
            .map((value) => value.trim())
            .filter(Boolean),
        ),
      ],
    };
  if (
    isRecord(input.logs) &&
    typeof input.logs.preserveAcrossRestarts === "boolean"
  )
    output.logs = { preserveAcrossRestarts: input.logs.preserveAcrossRestarts };
  if (isRecord(input.preview) && typeof input.preview.autoPreview === "boolean")
    output.preview = { autoPreview: input.preview.autoPreview };
  if (isRecord(input.workbench) && typeof input.workbench.theme === "string")
    output.workbench = { theme: normalizeTheme(input.workbench.theme) };
  return output;
}

export function readUserSettings(): PartialSettings {
  try {
    return sanitizeSettings(
      JSON.parse(localStorage.getItem(USER_SETTINGS_KEY) ?? "null"),
    );
  } catch {
    return {};
  }
}

export function resolveSettings(
  userInput: unknown,
  workspaceInput: unknown,
): FullSettings {
  const user = sanitizeSettings(userInput);
  const workspace = sanitizeSettings(workspaceInput);
  return {
    editor: { ...DEFAULT_SETTINGS.editor, ...user.editor, ...workspace.editor },
    files: { ...DEFAULT_SETTINGS.files, ...user.files, ...workspace.files },
    logs: { ...DEFAULT_SETTINGS.logs, ...user.logs, ...workspace.logs },
    preview: {
      ...DEFAULT_SETTINGS.preview,
      ...user.preview,
      ...workspace.preview,
    },
    workbench: {
      ...DEFAULT_SETTINGS.workbench,
      ...user.workbench,
      ...workspace.workbench,
    },
  };
}

export function updatePartialSettings<
  Group extends keyof FullSettings,
  Name extends keyof FullSettings[Group],
>(
  source: PartialSettings,
  group: Group,
  name: Name,
  value: FullSettings[Group][Name],
): PartialSettings {
  return sanitizeSettings({
    ...source,
    [group]: { ...source[group], [name]: value },
  });
}

export function createSettingsSnapshot(
  userInput: unknown,
  workspaceInput: unknown,
  hasWorkspace: boolean,
): SettingsSnapshot {
  const user = sanitizeSettings(userInput);
  const workspace = sanitizeSettings(workspaceInput);
  return {
    user,
    workspace,
    resolved: { settings: resolveSettings(user, workspace) },
    hasWorkspace,
  };
}

export function isPathExcluded(path: string, patterns: string[]) {
  return patterns.some((pattern) => globRegex(pattern).test(path));
}

function globRegex(pattern: string) {
  let source = "";
  for (let index = 0; index < pattern.length; index += 1) {
    const character = pattern[index];
    if (character === "*" && pattern[index + 1] === "*") {
      const followedBySlash = pattern[index + 2] === "/";
      source += followedBySlash ? "(?:.*/)?" : ".*";
      index += followedBySlash ? 2 : 1;
    } else if (character === "*") source += "[^/]*";
    else if (character === "?") source += "[^/]";
    else source += character.replace(/[\\^$+?.()|[\]{}]/g, "\\$&");
  }
  return new RegExp(
    `${pattern.includes("/") ? "^" : "^(?:.*/)?"}${source}(?:/.*)?$`,
  );
}
