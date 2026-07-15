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
import { ScrollArea } from "@edger/ui/components/ui/scroll-area";
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
import {
  ListFilterIcon,
  PlusIcon,
  RotateCcwIcon,
  SearchIcon,
  Trash2Icon,
} from "@edger/ui/icons/lucide";
import {
  filterSettingDefinitions,
  getSettingValueForScope,
  isDefinitionModified,
  SETTINGS_CATEGORIES,
  SETTINGS_DEFINITIONS,
  type FullSettings,
  type SettingDefinition,
  type SettingsCategory,
  type SettingsScope,
  type SettingsSnapshot,
} from "../lib/settings";

type ThemePreference = "dark" | "light" | "system";

const THEME_OPTIONS: { label: string; value: ThemePreference }[] = [
  { label: "System", value: "system" },
  { label: "Light", value: "light" },
  { label: "Dark", value: "dark" },
];

type SettingsBridge = {
  getSnapshot(): SettingsSnapshot;
  reset?<
    Group extends keyof FullSettings,
    Name extends keyof FullSettings[Group],
  >(
    scope: SettingsScope,
    group: Group,
    name: Name,
  ): void;
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
  definition: SettingDefinition,
) {
  if (isDefinitionModified(snapshot, scope, definition))
    return scope === "user" ? "Modified in User" : "Modified in Workspace";
  if (
    scope === "workspace" &&
    isDefinitionModified(snapshot, "user", definition)
  )
    return "Inherited from User";
  return "Default value";
}

function SettingsRow({
  children,
  definition,
  hint,
  modified,
  onReset,
}: {
  children: React.ReactNode;
  definition: SettingDefinition;
  hint: string;
  modified: boolean;
  onReset(): void;
}) {
  return (
    <div className="group flex flex-col gap-3 py-4 sm:flex-row sm:items-center sm:justify-between sm:gap-8">
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex items-center gap-2">
          <Label htmlFor={`settings-${definition.id}`}>
            {definition.label}
          </Label>
          {modified && (
            <Button
              aria-label={`Reset ${definition.label}`}
              className="opacity-70 sm:opacity-0 sm:group-hover:opacity-100 sm:focus-visible:opacity-100"
              onClick={onReset}
              size="icon-xs"
              title="Reset to inherited value"
              type="button"
              variant="ghost"
            >
              <RotateCcwIcon />
            </Button>
          )}
        </div>
        <p className="text-sm text-muted-foreground">
          {definition.description}
        </p>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs">
          <span className={modified ? "text-primary" : "text-muted-foreground"}>
            {hint}
          </span>
          <code className="text-muted-foreground/70">{definition.id}</code>
        </div>
      </div>
      <div className="w-full shrink-0 sm:w-72">{children}</div>
    </div>
  );
}

type SettingsDialogProps = {
  onOpenChange?: (open: boolean) => void;
  onReset?: <
    Group extends keyof FullSettings,
    Name extends keyof FullSettings[Group],
  >(
    scope: SettingsScope,
    group: Group,
    name: Name,
  ) => void;
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
  onReset,
  onSet,
  open: controlledOpen,
  snapshot: controlledSnapshot,
}: SettingsDialogProps = {}) {
  const [internalOpen, setInternalOpen] = React.useState(false);
  const [scope, setScope] = React.useState<SettingsScope>("user");
  const [bridgeSnapshot, setBridgeSnapshot] = React.useState(readSnapshot);
  const [category, setCategory] =
    React.useState<SettingsCategory>("editor");
  const [query, setQuery] = React.useState("");
  const [modifiedOnly, setModifiedOnly] = React.useState(false);
  const [pattern, setPattern] = React.useState("");
  const [settingsViewport, setSettingsViewport] =
    React.useState<HTMLDivElement | null>(null);
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
  }, [controlledOpen, refresh, setOpen]);

  const definitions = React.useMemo(
    () =>
      filterSettingDefinitions(SETTINGS_DEFINITIONS, {
        modifiedOnly,
        query,
        scope,
        snapshot,
      }),
    [modifiedOnly, query, scope, snapshot],
  );

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

  function reset<
    Group extends keyof FullSettings,
    Name extends keyof FullSettings[Group],
  >(group: Group, name: Name) {
    if (onReset) onReset(scope, group, name);
    else {
      window.edgerWebIdeSettings?.reset?.(scope, group, name);
      refresh();
    }
  }

  function renderControl(definition: SettingDefinition) {
    const modified = isDefinitionModified(snapshot, scope, definition);
    const row = (children: React.ReactNode, onReset: () => void) => (
      <SettingsRow
        definition={definition}
        hint={originHint(snapshot, scope, definition)}
        modified={modified}
        onReset={onReset}
      >
        {children}
      </SettingsRow>
    );

    switch (definition.id) {
      case "editor.fontSize":
        return row(
          <Input
            id={`settings-${definition.id}`}
            max={32}
            min={8}
            onChange={(event) =>
              set("editor", "fontSize", event.currentTarget.valueAsNumber)
            }
            type="number"
            value={getSettingValueForScope(
              snapshot,
              scope,
              "editor",
              "fontSize",
            )}
          />,
          () => reset("editor", "fontSize"),
        );
      case "editor.fontFamily":
        return row(
          <Input
            id={`settings-${definition.id}`}
            onChange={(event) =>
              set("editor", "fontFamily", event.currentTarget.value)
            }
            value={getSettingValueForScope(
              snapshot,
              scope,
              "editor",
              "fontFamily",
            )}
          />,
          () => reset("editor", "fontFamily"),
        );
      case "editor.tabSize":
        return row(
          <Input
            id={`settings-${definition.id}`}
            max={8}
            min={1}
            onChange={(event) =>
              set("editor", "tabSize", event.currentTarget.valueAsNumber)
            }
            type="number"
            value={getSettingValueForScope(
              snapshot,
              scope,
              "editor",
              "tabSize",
            )}
          />,
          () => reset("editor", "tabSize"),
        );
      case "editor.wordWrap":
        return row(
          <div className="flex justify-end">
            <Switch
              checked={getSettingValueForScope(
                snapshot,
                scope,
                "editor",
                "wordWrap",
              )}
              id={`settings-${definition.id}`}
              onCheckedChange={(checked) =>
                set("editor", "wordWrap", checked)
              }
            />
          </div>,
          () => reset("editor", "wordWrap"),
        );
      case "editor.lineNumbers":
        return row(
          <div className="flex justify-end">
            <Switch
              checked={getSettingValueForScope(
                snapshot,
                scope,
                "editor",
                "lineNumbers",
              )}
              id={`settings-${definition.id}`}
              onCheckedChange={(checked) =>
                set("editor", "lineNumbers", checked)
              }
            />
          </div>,
          () => reset("editor", "lineNumbers"),
        );
      case "workbench.theme":
        return row(
          <Select
            items={THEME_OPTIONS}
            onValueChange={(value) =>
              set("workbench", "theme", value as ThemePreference)
            }
            value={getSettingValueForScope(
              snapshot,
              scope,
              "workbench",
              "theme",
            )}
          >
            <SelectTrigger
              className="w-full"
              id={`settings-${definition.id}`}
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {THEME_OPTIONS.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>,
          () => reset("workbench", "theme"),
        );
      case "workbench.previewVisible":
      case "workbench.panelVisible": {
        const name = definition.name as
          | "previewVisible"
          | "panelVisible";
        return row(
          <div className="flex justify-end">
            <Switch
              checked={getSettingValueForScope(
                snapshot,
                scope,
                "workbench",
                name,
              )}
              id={`settings-${definition.id}`}
              onCheckedChange={(checked) =>
                set("workbench", name, checked)
              }
            />
          </div>,
          () => reset("workbench", name),
        );
      }
      case "logs.preserveAcrossRestarts":
        return row(
          <div className="flex justify-end">
            <Switch
              checked={getSettingValueForScope(
                snapshot,
                scope,
                "logs",
                "preserveAcrossRestarts",
              )}
              id={`settings-${definition.id}`}
              onCheckedChange={(checked) =>
                set("logs", "preserveAcrossRestarts", checked)
              }
            />
          </div>,
          () => reset("logs", "preserveAcrossRestarts"),
        );
      case "preview.autoPreview":
        return row(
          <div className="flex justify-end">
            <Switch
              checked={getSettingValueForScope(
                snapshot,
                scope,
                "preview",
                "autoPreview",
              )}
              id={`settings-${definition.id}`}
              onCheckedChange={(checked) =>
                set("preview", "autoPreview", checked)
              }
            />
          </div>,
          () => reset("preview", "autoPreview"),
        );
      case "files.autoSaveDelay":
        return row(
          <Input
            id={`settings-${definition.id}`}
            max={5000}
            min={100}
            onChange={(event) =>
              set("files", "autoSaveDelay", event.currentTarget.valueAsNumber)
            }
            step={50}
            type="number"
            value={getSettingValueForScope(
              snapshot,
              scope,
              "files",
              "autoSaveDelay",
            )}
          />,
          () => reset("files", "autoSaveDelay"),
        );
      case "files.exclude": {
        const exclude = getSettingValueForScope(
          snapshot,
          scope,
          "files",
          "exclude",
        );
        return row(
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
                        candidate === index ? event.currentTarget.value : item,
                      ),
                    )
                  }
                />
                <Button
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
                  size="icon"
                  type="button"
                  variant="ghost"
                >
                  <Trash2Icon />
                </Button>
              </div>
            ))}
            <div className="flex gap-2">
              <Input
                id={`settings-${definition.id}`}
                onChange={(event) => setPattern(event.currentTarget.value)}
                placeholder="node_modules/**"
                value={pattern}
              />
              <Button
                disabled={!pattern.trim()}
                onClick={() => {
                  set("files", "exclude", [...exclude, pattern.trim()]);
                  setPattern("");
                }}
                type="button"
                variant="outline"
              >
                <PlusIcon data-icon="inline-start" />
                Add
              </Button>
            </div>
          </div>,
          () => reset("files", "exclude"),
        );
      }
      default:
        return null;
    }
  }

  const visibleSections = React.useMemo(
    () =>
      SETTINGS_CATEGORIES.map((candidate) => ({
        ...candidate,
        definitions: definitions.filter(
          (definition) => definition.category === candidate.id,
        ),
      })).filter((section) => section.definitions.length > 0),
    [definitions],
  );
  const visibleSectionIds = visibleSections
    .map((section) => section.id)
    .join("|");

  const updateActiveCategory = React.useCallback(
    (viewport: HTMLDivElement) => {
      const sections = Array.from(
        viewport.querySelectorAll<HTMLElement>("[data-settings-category]"),
      );
      if (!sections.length) return;

      let activeSection = sections[0];
      const atBottom =
        Math.ceil(viewport.scrollTop + viewport.clientHeight) >=
        viewport.scrollHeight - 1;
      if (atBottom) activeSection = sections[sections.length - 1];
      else {
        const threshold =
          viewport.getBoundingClientRect().top + viewport.clientHeight * 0.3;
        for (const section of sections) {
          if (section.getBoundingClientRect().top > threshold) break;
          activeSection = section;
        }
      }

      const nextCategory = activeSection.dataset
        .settingsCategory as SettingsCategory;
      setCategory((current) =>
        current === nextCategory ? current : nextCategory,
      );
    },
    [],
  );

  React.useEffect(() => {
    if (!settingsViewport) return;
    const update = () => updateActiveCategory(settingsViewport);
    update();
    settingsViewport.addEventListener("scroll", update, { passive: true });
    return () => settingsViewport.removeEventListener("scroll", update);
  }, [settingsViewport, updateActiveCategory, visibleSectionIds]);

  function selectCategory(nextCategory: SettingsCategory) {
    setCategory(nextCategory);
    const viewport = settingsViewport;
    const section = viewport?.querySelector<HTMLElement>(
      `#settings-section-${nextCategory}`,
    );
    if (!viewport || !section) return;
    viewport.scrollTo({
      behavior: "smooth",
      top:
        viewport.scrollTop +
        section.getBoundingClientRect().top -
        viewport.getBoundingClientRect().top,
    });
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="h-[min(82vh,52rem)] grid-rows-[auto_minmax(0,1fr)] gap-0 overflow-hidden p-0 sm:max-w-5xl">
        <DialogHeader className="flex-row items-end gap-4 border-b px-5 py-4 pr-12">
          <div>
            <DialogTitle>Settings</DialogTitle>
            <DialogDescription className="mt-1">
              Configure the editor and workbench for yourself or this workspace.
            </DialogDescription>
          </div>
          <Tabs
            className="ml-auto shrink-0"
            onValueChange={(value) =>
              setScope(value === "workspace" ? "workspace" : "user")
            }
            value={scope}
          >
            <TabsList variant="segmented">
              <TabsTrigger value="user">User</TabsTrigger>
              <TabsTrigger disabled={!snapshot.hasWorkspace} value="workspace">
                Workspace
              </TabsTrigger>
            </TabsList>
          </Tabs>
        </DialogHeader>

        <div className="grid min-h-0 grid-rows-[auto_minmax(0,1fr)] md:grid-cols-[13rem_minmax(0,1fr)] md:grid-rows-1">
          <aside className="border-b bg-muted/25 md:border-r md:border-b-0">
            <div className="flex items-center gap-1 px-2 py-3">
              <div className="relative min-w-0 flex-1">
                <SearchIcon className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  aria-label="Search settings"
                  className="h-7 pl-9 text-[0.8rem] md:text-[0.8rem]"
                  onChange={(event) => setQuery(event.currentTarget.value)}
                  placeholder="Search..."
                  value={query}
                />
              </div>
              <Button
                aria-label="Show only settings modified in this scope"
                aria-pressed={modifiedOnly}
                onClick={() => setModifiedOnly((current) => !current)}
                size="icon-sm"
                title="Show only settings modified in this scope"
                type="button"
                variant={modifiedOnly ? "secondary" : "ghost"}
              >
                <ListFilterIcon />
              </Button>
            </div>
            <nav
              aria-label="Settings categories"
              className="flex gap-1 overflow-x-auto px-2 pb-2 md:flex-col md:overflow-visible"
            >
              {visibleSections.map((candidate) => (
                <Button
                  aria-current={category === candidate.id ? "page" : undefined}
                  className="justify-start"
                  key={candidate.id}
                  onClick={() => selectCategory(candidate.id)}
                  size="sm"
                  type="button"
                  variant={category === candidate.id ? "secondary" : "ghost"}
                >
                  {candidate.label}
                </Button>
              ))}
            </nav>
          </aside>

          <ScrollArea
            className="min-h-0"
            viewportClassName="px-5 py-3"
            viewportRef={setSettingsViewport}
          >
            {visibleSections.length ? (
              visibleSections.map((section, sectionIndex) => (
                <React.Fragment key={section.id}>
                  {sectionIndex > 0 && <Separator className="mb-3" />}
                  <section
                    data-settings-category={section.id}
                    id={`settings-section-${section.id}`}
                  >
                    <h3 className="pt-1 text-sm font-medium">
                      {section.label}
                    </h3>
                    {section.definitions.map((definition) => (
                      <React.Fragment key={definition.id}>
                        {renderControl(definition)}
                      </React.Fragment>
                    ))}
                  </section>
                </React.Fragment>
              ))
            ) : (
              <div className="grid h-full min-h-48 place-items-center text-center">
                <div>
                  <SearchIcon className="mx-auto mb-2 size-5 text-muted-foreground" />
                  <p className="font-medium">No settings found</p>
                  <p className="mt-1 text-sm text-muted-foreground">
                    Try another search or clear the Modified filter.
                  </p>
                </div>
              </div>
            )}
          </ScrollArea>
        </div>
      </DialogContent>
    </Dialog>
  );
}
