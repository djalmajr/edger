import * as React from "react";

import { Button } from "@edger/ui/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@edger/ui/components/ui/dialog";
import { Input } from "@edger/ui/components/ui/input";
import { Label } from "@edger/ui/components/ui/label";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@edger/ui/components/ui/select";
import { Separator } from "@edger/ui/components/ui/separator";
import { Switch } from "@edger/ui/components/ui/switch";
import { Tabs, TabsList, TabsTrigger } from "@edger/ui/components/ui/tabs";
import { PlusIcon, Trash2Icon } from "@edger/ui/icons/lucide";
import { useTheme } from "@edger/ui/lib/theme";
import type {
  FullSettings,
  SettingsScope,
  SettingsSnapshot,
} from "../lib/settings";

type ThemePreference = "dark" | "light" | "system";

type SettingsBridge = {
  getSnapshot(): SettingsSnapshot;
  set<Group extends keyof FullSettings, Name extends keyof FullSettings[Group]>(
    scope: SettingsScope,
    group: Group,
    name: Name,
    value: FullSettings[Group][Name],
  ): void;
};

declare global {
  interface Window {
    edgerWebIdeSettings?: SettingsBridge;
  }
}

const EMPTY_SNAPSHOT: SettingsSnapshot = {
  user: {},
  workspace: {},
  resolved: {
    settings: {
      editor: {
        fontFamily:
          "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
        fontSize: 14,
        tabSize: 2,
        wordWrap: false,
      },
      files: { exclude: ["node_modules/**"] },
      logs: { preserveAcrossRestarts: false },
      preview: { autoPreview: true },
      workbench: { theme: "system" },
    },
  },
  hasWorkspace: false,
};

function readSnapshot() {
  return window.edgerWebIdeSettings?.getSnapshot() ?? EMPTY_SNAPSHOT;
}

function originHint(
  snapshot: SettingsSnapshot,
  scope: SettingsScope,
  group: keyof FullSettings,
  name: string,
) {
  const scoped = snapshot[scope][group] as Record<string, unknown> | undefined;
  if (scoped?.[name] !== undefined)
    return scope === "user" ? "Modified in User" : "Modified in Workspace";
  if (scope === "workspace") {
    const user = snapshot.user[group] as Record<string, unknown> | undefined;
    if (user?.[name] !== undefined) return "Inherited from User";
  }
  return "Default value";
}

function SettingsRow({
  children,
  description,
  hint,
  id,
  label,
}: {
  children: React.ReactNode;
  description: string;
  hint: string;
  id: string;
  label: string;
}) {
  return (
    <div className="flex flex-col gap-3 py-4 sm:flex-row sm:items-center sm:justify-between sm:gap-8">
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <Label htmlFor={id}>{label}</Label>
        <p className="text-sm text-muted-foreground">{description}</p>
        <p className="text-xs text-primary">{hint}</p>
      </div>
      <div className="w-full shrink-0 sm:w-72">{children}</div>
    </div>
  );
}

function SettingsSection({
  children,
  title,
}: {
  children: React.ReactNode;
  title: string;
}) {
  return (
    <section className="flex flex-col">
      <h3 className="pt-2 text-sm font-medium">{title}</h3>
      <Separator className="mt-3" />
      {children}
    </section>
  );
}

type SettingsDialogProps = {
  onOpenChange?: (open: boolean) => void;
  onSet?: <
    Group extends keyof FullSettings,
    Name extends keyof FullSettings[Group],
  >(
    scope: SettingsScope,
    group: Group,
    name: Name,
    value: FullSettings[Group][Name],
  ) => void;
  open?: boolean;
  snapshot?: SettingsSnapshot;
};

export function SettingsDialog({
  onOpenChange,
  onSet,
  open: controlledOpen,
  snapshot: controlledSnapshot,
}: SettingsDialogProps = {}) {
  const [internalOpen, setInternalOpen] = React.useState(false);
  const [scope, setScope] = React.useState<SettingsScope>("user");
  const [bridgeSnapshot, setBridgeSnapshot] = React.useState(readSnapshot);
  const [pattern, setPattern] = React.useState("");
  const { setTheme } = useTheme();
  const open = controlledOpen ?? internalOpen;
  const snapshot = controlledSnapshot ?? bridgeSnapshot;
  const setOpen = React.useCallback(
    (next: boolean) => {
      if (controlledOpen === undefined) setInternalOpen(next);
      onOpenChange?.(next);
    },
    [controlledOpen, onOpenChange],
  );

  const refresh = React.useCallback(
    () => setBridgeSnapshot(readSnapshot()),
    [],
  );

  React.useEffect(() => {
    if (controlledOpen !== undefined) return;
    const show = () => {
      refresh();
      setOpen(true);
    };
    window.addEventListener("edger:open-settings", show);
    window.addEventListener("edger:settings-changed", refresh);
    return () => {
      window.removeEventListener("edger:open-settings", show);
      window.removeEventListener("edger:settings-changed", refresh);
    };
  }, [controlledOpen, refresh]);

  React.useEffect(() => {
    setTheme(snapshot.resolved.settings.workbench.theme);
  }, [setTheme, snapshot.resolved.settings.workbench.theme]);

  const scoped = snapshot[scope];
  const settings = snapshot.resolved.settings;

  function set<
    Group extends keyof FullSettings,
    Name extends keyof FullSettings[Group],
  >(group: Group, name: Name, value: FullSettings[Group][Name]) {
    if (onSet) onSet(scope, group, name, value);
    else {
      window.edgerWebIdeSettings?.set(scope, group, name, value);
      refresh();
    }
  }

  const exclude = scoped.files?.exclude ?? settings.files.exclude;

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader className="pr-10">
          <DialogTitle>Settings</DialogTitle>
          <DialogDescription>
            Configure editor, workbench, preview, logs, and file visibility.
          </DialogDescription>
        </DialogHeader>

        <Tabs
          value={scope}
          onValueChange={(value) =>
            setScope(value === "workspace" ? "workspace" : "user")
          }
        >
          <TabsList variant="line" className="ml-auto">
            <TabsTrigger value="user">User</TabsTrigger>
            <TabsTrigger value="workspace" disabled={!snapshot.hasWorkspace}>
              Workspace
            </TabsTrigger>
          </TabsList>
        </Tabs>

        <div className="flex max-h-[min(70vh,46rem)] flex-col overflow-y-auto pr-1">
          <SettingsSection title="Editor">
            <SettingsRow
              id="settings-font-size"
              label="Font Size"
              description="Editor text size in pixels."
              hint={originHint(snapshot, scope, "editor", "fontSize")}
            >
              <Input
                id="settings-font-size"
                type="number"
                min={8}
                max={32}
                value={settings.editor.fontSize}
                onChange={(event) =>
                  set("editor", "fontSize", event.currentTarget.valueAsNumber)
                }
              />
            </SettingsRow>
            <Separator />
            <SettingsRow
              id="settings-font-family"
              label="Font Family"
              description="Font stack used by the code editor."
              hint={originHint(snapshot, scope, "editor", "fontFamily")}
            >
              <Input
                id="settings-font-family"
                value={settings.editor.fontFamily}
                onChange={(event) =>
                  set("editor", "fontFamily", event.currentTarget.value)
                }
              />
            </SettingsRow>
            <Separator />
            <SettingsRow
              id="settings-tab-size"
              label="Tab Size"
              description="Spaces inserted for indentation."
              hint={originHint(snapshot, scope, "editor", "tabSize")}
            >
              <Input
                id="settings-tab-size"
                type="number"
                min={1}
                max={8}
                value={settings.editor.tabSize}
                onChange={(event) =>
                  set("editor", "tabSize", event.currentTarget.valueAsNumber)
                }
              />
            </SettingsRow>
            <Separator />
            <SettingsRow
              id="settings-word-wrap"
              label="Word Wrap"
              description="Wrap long editor lines within the viewport."
              hint={originHint(snapshot, scope, "editor", "wordWrap")}
            >
              <div className="flex justify-end">
                <Switch
                  id="settings-word-wrap"
                  checked={settings.editor.wordWrap}
                  onCheckedChange={(checked) =>
                    set("editor", "wordWrap", checked)
                  }
                />
              </div>
            </SettingsRow>
          </SettingsSection>

          <SettingsSection title="Workbench">
            <SettingsRow
              id="settings-theme"
              label="Theme"
              description="Preferred workbench color theme."
              hint={originHint(snapshot, scope, "workbench", "theme")}
            >
              <Select
                value={settings.workbench.theme}
                onValueChange={(value) =>
                  set("workbench", "theme", value as ThemePreference)
                }
              >
                <SelectTrigger id="settings-theme" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="system">System</SelectItem>
                    <SelectItem value="light">Light</SelectItem>
                    <SelectItem value="dark">Dark</SelectItem>
                  </SelectGroup>
                </SelectContent>
              </Select>
            </SettingsRow>
          </SettingsSection>

          <SettingsSection title="Preview & Logs">
            <SettingsRow
              id="settings-auto-preview"
              label="Auto Preview"
              description="Automatically refresh preview after file changes."
              hint={originHint(snapshot, scope, "preview", "autoPreview")}
            >
              <div className="flex justify-end">
                <Switch
                  id="settings-auto-preview"
                  checked={settings.preview.autoPreview}
                  onCheckedChange={(checked) =>
                    set("preview", "autoPreview", checked)
                  }
                />
              </div>
            </SettingsRow>
            <Separator />
            <SettingsRow
              id="settings-preserve-logs"
              label="Preserve Across Restarts"
              description="Keep log output when the preview restarts."
              hint={originHint(
                snapshot,
                scope,
                "logs",
                "preserveAcrossRestarts",
              )}
            >
              <div className="flex justify-end">
                <Switch
                  id="settings-preserve-logs"
                  checked={settings.logs.preserveAcrossRestarts}
                  onCheckedChange={(checked) =>
                    set("logs", "preserveAcrossRestarts", checked)
                  }
                />
              </div>
            </SettingsRow>
          </SettingsSection>

          <SettingsSection title="Files">
            <div className="flex flex-col gap-3 py-4">
              <div className="flex flex-col gap-1">
                <Label htmlFor="settings-exclude-pattern">Exclude</Label>
                <p className="text-sm text-muted-foreground">
                  Glob patterns hidden from file-oriented surfaces.
                </p>
                <p className="text-xs text-primary">
                  {originHint(snapshot, scope, "files", "exclude")}
                </p>
              </div>
              <div className="flex flex-col gap-2">
                {exclude.map((value, index) => (
                  <div className="flex gap-2" key={`${value}-${index}`}>
                    <Input
                      aria-label={`Exclude pattern ${index + 1}`}
                      value={value}
                      onChange={(event) =>
                        set(
                          "files",
                          "exclude",
                          exclude.map((item, candidate) =>
                            candidate === index
                              ? event.currentTarget.value
                              : item,
                          ),
                        )
                      }
                    />
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      aria-label={`Remove ${value}`}
                      onClick={() =>
                        set(
                          "files",
                          "exclude",
                          exclude.filter(
                            (_item, candidate) => candidate !== index,
                          ),
                        )
                      }
                    >
                      <Trash2Icon />
                    </Button>
                  </div>
                ))}
                <div className="flex gap-2">
                  <Input
                    id="settings-exclude-pattern"
                    placeholder="node_modules/**"
                    value={pattern}
                    onChange={(event) => setPattern(event.currentTarget.value)}
                  />
                  <Button
                    type="button"
                    variant="outline"
                    disabled={!pattern.trim()}
                    onClick={() => {
                      set("files", "exclude", [...exclude, pattern.trim()]);
                      setPattern("");
                    }}
                  >
                    <PlusIcon data-icon="inline-start" />
                    Add pattern
                  </Button>
                </div>
              </div>
            </div>
          </SettingsSection>
        </div>
      </DialogContent>
    </Dialog>
  );
}
