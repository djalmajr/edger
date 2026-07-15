import { normalizeTheme, type ThemePreference } from "@edger/ui/lib/theme";

export type FullSettings = {
  editor: {
    fontFamily: string;
    fontSize: number;
    lineNumbers: boolean;
    tabSize: number;
    wordWrap: boolean;
  };
  files: { autoSaveDelay: number; exclude: string[] };
  logs: { preserveAcrossRestarts: boolean };
  preview: { autoPreview: boolean };
  workbench: {
    panelVisible: boolean;
    previewVisible: boolean;
    theme: ThemePreference;
  };
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
export type SettingsCategory =
  | "editor"
  | "workbench"
  | "preview-logs"
  | "files";
export type SettingDefinition = {
  category: SettingsCategory;
  description: string;
  group: keyof FullSettings;
  id: string;
  keywords: readonly string[];
  label: string;
  name: string;
  scopes: readonly SettingsScope[];
};

export const USER_SETTINGS_KEY = "edger.webide.userSettings";
export const DEFAULT_SETTINGS: FullSettings = {
  editor: {
    fontFamily: '"SFMono-Regular", Consolas, monospace',
    fontSize: 14,
    lineNumbers: true,
    tabSize: 2,
    wordWrap: false,
  },
  files: { autoSaveDelay: 350, exclude: [] },
  logs: { preserveAcrossRestarts: false },
  preview: { autoPreview: true },
  workbench: {
    panelVisible: true,
    previewVisible: true,
    theme: "system",
  },
};
export const SETTINGS_CATEGORIES: ReadonlyArray<{
  id: SettingsCategory;
  label: string;
}> = [
  { id: "editor", label: "Editor" },
  { id: "workbench", label: "Workbench" },
  { id: "preview-logs", label: "Preview & Logs" },
  { id: "files", label: "Files" },
];
export const SETTINGS_DEFINITIONS: readonly SettingDefinition[] = [
  {
    id: "editor.fontSize",
    category: "editor",
    group: "editor",
    name: "fontSize",
    label: "Font Size",
    description: "Editor text size in pixels.",
    keywords: ["text", "zoom", "typography"],
    scopes: ["user", "workspace"],
  },
  {
    id: "editor.fontFamily",
    category: "editor",
    group: "editor",
    name: "fontFamily",
    label: "Font Family",
    description: "Font stack used by the code editor.",
    keywords: ["typeface", "monospace", "typography"],
    scopes: ["user", "workspace"],
  },
  {
    id: "editor.tabSize",
    category: "editor",
    group: "editor",
    name: "tabSize",
    label: "Tab Size",
    description: "Spaces inserted for indentation.",
    keywords: ["indent", "spaces", "formatting"],
    scopes: ["user", "workspace"],
  },
  {
    id: "editor.lineNumbers",
    category: "editor",
    group: "editor",
    name: "lineNumbers",
    label: "Line Numbers",
    description: "Show line numbers when word wrap is off.",
    keywords: ["gutter", "lines", "editor"],
    scopes: ["user", "workspace"],
  },
  {
    id: "editor.wordWrap",
    category: "editor",
    group: "editor",
    name: "wordWrap",
    label: "Word Wrap",
    description: "Wrap long lines within the editor viewport.",
    keywords: ["long lines", "columns", "overflow"],
    scopes: ["user", "workspace"],
  },
  {
    id: "workbench.theme",
    category: "workbench",
    group: "workbench",
    name: "theme",
    label: "Theme",
    description: "Preferred workbench color theme.",
    keywords: ["dark", "light", "system", "appearance"],
    scopes: ["user"],
  },
  {
    id: "workbench.previewVisible",
    category: "workbench",
    group: "workbench",
    name: "previewVisible",
    label: "Show Preview",
    description: "Show the preview panel when the workbench opens.",
    keywords: ["layout", "panel", "browser"],
    scopes: ["user"],
  },
  {
    id: "workbench.panelVisible",
    category: "workbench",
    group: "workbench",
    name: "panelVisible",
    label: "Show Bottom Panel",
    description: "Show logs, terminal, deployments, and problems.",
    keywords: ["layout", "footer", "terminal", "logs"],
    scopes: ["user"],
  },
  {
    id: "logs.preserveAcrossRestarts",
    category: "preview-logs",
    group: "logs",
    name: "preserveAcrossRestarts",
    label: "Preserve Across Restarts",
    description: "Keep local log output when a deploy restarts the preview.",
    keywords: ["deploy", "console", "history"],
    scopes: ["user", "workspace"],
  },
  {
    id: "preview.autoPreview",
    category: "preview-logs",
    group: "preview",
    name: "autoPreview",
    label: "Auto Preview",
    description: "Open and refresh the preview after a successful deploy.",
    keywords: ["deploy", "refresh", "open", "browser"],
    scopes: ["user", "workspace"],
  },
  {
    id: "files.autoSaveDelay",
    category: "files",
    group: "files",
    name: "autoSaveDelay",
    label: "Auto Save Delay",
    description: "Wait time in milliseconds before saving local changes.",
    keywords: ["autosave", "delay", "draft", "debounce"],
    scopes: ["user", "workspace"],
  },
  {
    id: "files.exclude",
    category: "files",
    group: "files",
    name: "exclude",
    label: "Exclude",
    description: "Glob patterns hidden from Explorer and Search.",
    keywords: ["glob", "hide", "search", "explorer"],
    scopes: ["user", "workspace"],
  },
];

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
    if (typeof input.editor.lineNumbers === "boolean")
      editor.lineNumbers = input.editor.lineNumbers;
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
  if (isRecord(input.files)) {
    const files: Partial<FullSettings["files"]> = {};
    if (
      typeof input.files.autoSaveDelay === "number" &&
      input.files.autoSaveDelay >= 100 &&
      input.files.autoSaveDelay <= 5000
    )
      files.autoSaveDelay = input.files.autoSaveDelay;
    if (Array.isArray(input.files.exclude))
      files.exclude = [
        ...new Set(
          input.files.exclude
            .filter((value): value is string => typeof value === "string")
            .map((value) => value.trim())
            .filter(Boolean),
        ),
      ];
    if (Object.keys(files).length) output.files = files;
  }
  if (
    isRecord(input.logs) &&
    typeof input.logs.preserveAcrossRestarts === "boolean"
  )
    output.logs = { preserveAcrossRestarts: input.logs.preserveAcrossRestarts };
  if (isRecord(input.preview) && typeof input.preview.autoPreview === "boolean")
    output.preview = { autoPreview: input.preview.autoPreview };
  if (isRecord(input.workbench)) {
    const workbench: Partial<FullSettings["workbench"]> = {};
    if (typeof input.workbench.panelVisible === "boolean")
      workbench.panelVisible = input.workbench.panelVisible;
    if (typeof input.workbench.previewVisible === "boolean")
      workbench.previewVisible = input.workbench.previewVisible;
    if (typeof input.workbench.theme === "string")
      workbench.theme = normalizeTheme(input.workbench.theme);
    if (Object.keys(workbench).length) output.workbench = workbench;
  }
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

export function unsetPartialSetting<
  Group extends keyof FullSettings,
  Name extends keyof FullSettings[Group],
>(source: PartialSettings, group: Group, name: Name): PartialSettings {
  const next = structuredClone(source) as PartialSettings;
  const values = next[group] as Record<string, unknown> | undefined;
  if (!values) return sanitizeSettings(next);
  delete values[String(name)];
  if (!Object.keys(values).length) delete next[group];
  return sanitizeSettings(next);
}

export function getSettingValueForScope<
  Group extends keyof FullSettings,
  Name extends keyof FullSettings[Group],
>(
  snapshot: SettingsSnapshot,
  scope: SettingsScope,
  group: Group,
  name: Name,
): FullSettings[Group][Name] {
  const scoped = snapshot[scope][group] as
    | Partial<FullSettings[Group]>
    | undefined;
  const scopedValue = scoped?.[name];
  if (scopedValue !== undefined) return scopedValue;
  if (scope === "workspace") {
    const user = snapshot.user[group] as
      | Partial<FullSettings[Group]>
      | undefined;
    const userValue = user?.[name];
    if (userValue !== undefined) return userValue;
  }
  return DEFAULT_SETTINGS[group][name];
}

export function isSettingModified<
  Group extends keyof FullSettings,
  Name extends keyof FullSettings[Group],
>(
  snapshot: SettingsSnapshot,
  scope: SettingsScope,
  group: Group,
  name: Name,
) {
  const scoped = snapshot[scope][group] as
    | Partial<FullSettings[Group]>
    | undefined;
  return scoped?.[name] !== undefined;
}

export function isDefinitionModified(
  snapshot: SettingsSnapshot,
  scope: SettingsScope,
  definition: SettingDefinition,
) {
  const scoped = snapshot[scope][definition.group] as
    | Record<string, unknown>
    | undefined;
  return scoped?.[definition.name] !== undefined;
}

export function filterSettingDefinitions(
  definitions: readonly SettingDefinition[],
  options: {
    category?: SettingsCategory;
    modifiedOnly?: boolean;
    query: string;
    scope: SettingsScope;
    snapshot: SettingsSnapshot;
  },
) {
  const query = options.query.trim().toLocaleLowerCase();
  return definitions.filter((definition) => {
    if (!definition.scopes.includes(options.scope)) return false;
    if (options.category && definition.category !== options.category)
      return false;
    if (
      options.modifiedOnly &&
      !isDefinitionModified(options.snapshot, options.scope, definition)
    )
      return false;
    if (!query) return true;
    return [
      definition.id,
      definition.label,
      definition.description,
      ...definition.keywords,
    ]
      .join(" ")
      .toLocaleLowerCase()
      .includes(query);
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
