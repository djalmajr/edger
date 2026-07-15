import { useMutation, useQuery } from "@tanstack/react-query";
import * as React from "react";

import { Badge } from "@edger/ui/components/ui/badge";
import { Button } from "@edger/ui/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@edger/ui/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@edger/ui/components/ui/dropdown-menu";
import { Input } from "@edger/ui/components/ui/input";
import {
  InputGroup,
  InputGroupButton,
  InputGroupInput,
} from "@edger/ui/components/ui/input-group";
import { Label } from "@edger/ui/components/ui/label";
import { ScrollArea } from "@edger/ui/components/ui/scroll-area";
import {
  Sortable,
  SortableContent,
  SortableItem,
  SortableItemHandle,
} from "@edger/ui/components/ui/sortable";
import { Switch } from "@edger/ui/components/ui/switch";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@edger/ui/components/ui/tooltip";
import {
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CircleAlertIcon,
  Code2Icon,
  ExternalLinkIcon,
  EyeIcon,
  FileIcon,
  FilePlusIcon,
  FolderIcon,
  FolderOpenIcon,
  FolderPlusIcon,
  ListIcon,
  MoreVerticalIcon,
  PanelBottomIcon,
  RefreshCwIcon,
  SearchIcon,
  SettingsIcon,
  TerminalIcon,
  Trash2Icon,
  UploadIcon,
  WebhookIcon,
  XIcon,
} from "@edger/ui/icons/lucide";
import { useTheme } from "@edger/ui/lib/theme";
import { cn } from "@edger/ui/lib/utils";

import {
  highlightCode,
  languageFor,
  lintDocument,
  searchProjectFiles,
} from "../editor-tools.js";
import {
  createSettingsSnapshot,
  isPathExcluded,
  readUserSettings,
  resolveSettings,
  type FullSettings,
  type PartialSettings,
  type SettingsScope,
  unsetPartialSetting,
  updatePartialSettings,
  USER_SETTINGS_KEY,
} from "../lib/settings";
import {
  loadProjects,
  type Project,
  saveProject,
  validateFilePath,
  validateProject,
} from "../lib/projects";
import { SettingsDialog } from "./settings-dialog";

const FOOTER_TABS = ["problems", "logs", "terminal", "deployments"] as const;
type FooterTab = (typeof FOOTER_TABS)[number];
type FileDialog = {
  kind: "file" | "folder" | "rename-file" | "rename-folder";
  path?: string;
} | null;

function readStored<T>(key: string, fallback: T): T {
  try {
    return JSON.parse(localStorage.getItem(key) ?? "null") ?? fallback;
  } catch {
    return fallback;
  }
}

function workbenchStateKey(id: string) {
  return `edger.webide.workbench.${id}`;
}

function FileTypeIcon({ path }: { path: string }) {
  const extension = path.split(".").pop()?.toLowerCase();
  if (["html", "htm"].includes(extension ?? ""))
    return <Code2Icon className="size-4 text-orange-500" />;
  if (["yaml", "yml", "json"].includes(extension ?? ""))
    return <ListIcon className="size-4 text-rose-500" />;
  if (["js", "jsx", "mjs", "ts", "tsx"].includes(extension ?? ""))
    return <Code2Icon className="size-4 text-amber-500" />;
  return <FileIcon className="size-4 text-muted-foreground" />;
}

function ActionButton({
  label,
  pressed,
  children,
  ...props
}: React.ComponentProps<typeof Button> & { label: string; pressed?: boolean }) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <Button
            aria-label={label}
            aria-pressed={pressed}
            className={pressed ? "bg-accent text-accent-foreground" : undefined}
            size="icon-sm"
            variant="ghost"
            {...props}
          />
        }
      >
        {children}
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}

type TreeNode = {
  children: TreeNode[];
  folder: boolean;
  name: string;
  path: string;
};
function buildTree(project: Project, excluded: string[]): TreeNode[] {
  const root: TreeNode = { children: [], folder: true, name: "", path: "" };
  const folders = new Set(project.folders);
  Object.keys(project.files).forEach((path) =>
    path
      .split("/")
      .slice(0, -1)
      .forEach((_part, index, parts) =>
        folders.add(parts.slice(0, index + 1).join("/")),
      ),
  );
  const nodes = new Map<string, TreeNode>([["", root]]);
  [...folders].sort().forEach((path) => {
    if (isPathExcluded(path, excluded)) return;
    const parts = path.split("/");
    const node = {
      children: [],
      folder: true,
      name: parts.at(-1) ?? path,
      path,
    };
    nodes.set(path, node);
    nodes.get(parts.slice(0, -1).join("/"))?.children.push(node);
  });
  Object.keys(project.files)
    .sort()
    .forEach((path) => {
      if (isPathExcluded(path, excluded)) return;
      const parts = path.split("/");
      nodes.get(parts.slice(0, -1).join("/"))?.children.push({
        children: [],
        folder: false,
        name: parts.at(-1) ?? path,
        path,
      });
    });
  const sort = (children: TreeNode[]): TreeNode[] =>
    children
      .sort(
        (a, b) =>
          Number(b.folder) - Number(a.folder) || a.name.localeCompare(b.name),
      )
      .map((node): TreeNode => ({ ...node, children: sort(node.children) }));
  return sort(root.children);
}

function crc32(bytes: Uint8Array) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let index = 0; index < 8; index += 1)
      crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  return (crc ^ 0xffffffff) >>> 0;
}
function u16(value: number) {
  return [value & 255, (value >>> 8) & 255];
}
function u32(value: number) {
  return [...u16(value), ...u16(value >>> 16)];
}
function deterministicZip(files: Record<string, string>) {
  const encoder = new TextEncoder();
  const local: number[] = [];
  const central: number[] = [];
  let offset = 0;
  const entries = Object.entries(files).sort(([a], [b]) => a.localeCompare(b));
  for (const [name, content] of entries) {
    const filename = encoder.encode(name);
    const data = encoder.encode(content);
    const crc = crc32(data);
    const header = [
      ...u32(0x04034b50),
      ...u16(20),
      ...u16(0x0800),
      ...u16(0),
      ...u16(0),
      ...u16(33),
      ...u32(crc),
      ...u32(data.length),
      ...u32(data.length),
      ...u16(filename.length),
      ...u16(0),
      ...filename,
    ];
    local.push(...header, ...data);
    central.push(
      ...u32(0x02014b50),
      ...u16(20),
      ...u16(20),
      ...u16(0x0800),
      ...u16(0),
      ...u16(0),
      ...u16(33),
      ...u32(crc),
      ...u32(data.length),
      ...u32(data.length),
      ...u16(filename.length),
      ...u16(0),
      ...u16(0),
      ...u16(0),
      ...u16(0),
      ...u32(0),
      ...u32(offset),
      ...filename,
    );
    offset += header.length + data.length;
  }
  const end = [
    ...u32(0x06054b50),
    ...u16(0),
    ...u16(0),
    ...u16(entries.length),
    ...u16(entries.length),
    ...u32(central.length),
    ...u32(local.length),
    ...u16(0),
  ];
  return new Blob([new Uint8Array([...local, ...central, ...end])], {
    type: "application/zip",
  });
}

export function Workbench({
  projectId,
  onHome,
}: {
  projectId: string;
  onHome(): void;
}) {
  const projectsQuery = useQuery({
    queryKey: ["webide", "projects"],
    queryFn: loadProjects,
  });
  const sourceProject = projectsQuery.data?.find(
    (candidate) => candidate.id === projectId,
  );
  if (projectsQuery.isLoading)
    return (
      <div className="grid min-h-screen place-items-center text-muted-foreground">
        Opening workspace…
      </div>
    );
  if (!sourceProject)
    return (
      <div className="grid min-h-screen place-items-center gap-3">
        <p>Project not found.</p>
        <Button onClick={onHome}>Back to dashboard</Button>
      </div>
    );
  return (
    <LoadedWorkbench
      key={sourceProject.id}
      initialProject={sourceProject}
      onHome={onHome}
    />
  );
}

function LoadedWorkbench({
  initialProject,
  onHome,
}: {
  initialProject: Project;
  onHome(): void;
}) {
  const saved = readStored<{ openTabs?: string[]; selected?: string }>(
    workbenchStateKey(initialProject.id),
    {},
  );
  const initialSelected =
    saved.selected && initialProject.files[saved.selected] !== undefined
      ? saved.selected
      : initialProject.selected;
  const initialTabs = (
    saved.openTabs ??
    initialProject.openTabs ?? [initialSelected]
  ).filter((path) => initialProject.files[path] !== undefined);
  const [project, setProject] = React.useState(() =>
    structuredClone(initialProject),
  );
  const [selected, setSelected] = React.useState(
    initialTabs.includes(initialSelected)
      ? initialSelected
      : (initialTabs[0] ?? Object.keys(project.files)[0]),
  );
  const [openTabs, setOpenTabs] = React.useState(
    initialTabs.length ? initialTabs : [selected],
  );
  const [footerOrder, setFooterOrder] = React.useState<FooterTab[]>(() => {
    const stored = readStored<FooterTab[]>("edger.webide.footerOrder", [
      ...FOOTER_TABS,
    ]);
    return [
      ...new Set([
        ...stored.filter((tab) => FOOTER_TABS.includes(tab)),
        ...FOOTER_TABS,
      ]),
    ] as FooterTab[];
  });
  const [footerTab, setFooterTab] = React.useState<FooterTab>("logs");
  const [footerVisible, setFooterVisible] = React.useState(
    () => localStorage.getItem("edger.webide.footerVisible") !== "false",
  );
  const [previewVisible, setPreviewVisible] = React.useState(
    () => localStorage.getItem("edger.webide.previewVisible") !== "false",
  );
  const [sidebar, setSidebar] = React.useState<"files" | "search">("files");
  const [search, setSearch] = React.useState("");
  const [caseSensitive, setCaseSensitive] = React.useState(false);
  const [regex, setRegex] = React.useState(false);
  const [collapsed, setCollapsed] = React.useState<Set<string>>(
    () => new Set(),
  );
  const [settingsOpen, setSettingsOpen] = React.useState(false);
  const [userSettings, setUserSettings] = React.useState(readUserSettings);
  const [fileDialog, setFileDialog] = React.useState<FileDialog>(null);
  const [fileDialogValue, setFileDialogValue] = React.useState("");
  const [fileDialogError, setFileDialogError] = React.useState("");
  const [terminalHistory, setTerminalHistory] = React.useState<
    Array<{ kind?: string; prompt?: boolean; text: string }>
  >([]);
  const [terminalInput, setTerminalInput] = React.useState("");
  const [message, setMessage] = React.useState("Draft ready");
  const [refreshKey, setRefreshKey] = React.useState(0);
  const [deploySteps, setDeploySteps] = React.useState<
    Array<{ label: string; status: string }>
  >([]);
  const { setTheme } = useTheme();
  const saveReady = React.useRef(false);
  const editorRef = React.useRef<HTMLTextAreaElement>(null);
  const syntaxRef = React.useRef<HTMLPreElement>(null);
  const linesRef = React.useRef<HTMLDivElement>(null);

  const settings = React.useMemo(
    () => resolveSettings(userSettings, project.settings),
    [project.settings, userSettings],
  );
  const snapshot = React.useMemo(
    () => createSettingsSnapshot(userSettings, project.settings, true),
    [project.settings, userSettings],
  );
  const source = project.files[selected] ?? "";
  const diagnostics = React.useMemo(
    () => lintDocument(selected, source, project.files),
    [project.files, selected, source],
  );
  const searchableFiles = React.useMemo(
    () =>
      Object.fromEntries(
        Object.entries(project.files).filter(
          ([path]) => !isPathExcluded(path, settings.files.exclude),
        ),
      ),
    [project.files, settings.files.exclude],
  );
  const searchResult = React.useMemo(
    () => searchProjectFiles(searchableFiles, search, { caseSensitive, regex }),
    [caseSensitive, regex, search, searchableFiles],
  );
  const tree = React.useMemo(
    () => buildTree(project, settings.files.exclude),
    [project, settings.files.exclude],
  );

  React.useEffect(() => {
    setTheme(settings.workbench.theme);
  }, [setTheme, settings.workbench.theme]);
  React.useEffect(() => {
    setPreviewVisible(settings.workbench.previewVisible);
  }, [settings.workbench.previewVisible]);
  React.useEffect(() => {
    setFooterVisible(settings.workbench.panelVisible);
  }, [settings.workbench.panelVisible]);
  React.useEffect(() => {
    document.documentElement.style.setProperty(
      "--editor-font-family",
      settings.editor.fontFamily,
    );
    document.documentElement.style.setProperty(
      "--editor-font-size",
      `${settings.editor.fontSize}px`,
    );
    document.documentElement.style.setProperty(
      "--editor-tab-size",
      String(settings.editor.tabSize),
    );
  }, [settings.editor]);
  React.useEffect(() => {
    localStorage.setItem(
      workbenchStateKey(project.id),
      JSON.stringify({ openTabs, selected }),
    );
    if (!saveReady.current) {
      saveReady.current = true;
      return;
    }
    setMessage("Unsaved local changes");
    const timer = window.setTimeout(
      () =>
        void saveProject({ ...project, openTabs, selected })
          .then(() => setMessage("Draft saved locally"))
          .catch((reason) =>
            setMessage(
              reason instanceof Error ? reason.message : String(reason),
            ),
          ),
      settings.files.autoSaveDelay,
    );
    return () => window.clearTimeout(timer);
  }, [openTabs, project, selected, settings.files.autoSaveDelay]);

  const deployMutation = useMutation({
    mutationFn: async () => {
      const next = structuredClone(project);
      const manifest = validateProject(next);
      const steps = [
        "Validation",
        "Packaging",
        "Upload",
        "Release / migrations",
        "Health check",
        "Activation",
        "Complete",
      ];
      setDeploySteps(
        steps.map((label, index) => ({
          label,
          status: index === 0 ? "active" : "pending",
        })),
      );
      if (!settings.logs.preserveAcrossRestarts) next.logs = [];
      next.logs.push({
        at: new Date().toISOString(),
        level: "info",
        message: "Manifest and project files validated.",
        source: "VALIDATE",
      });
      setDeploySteps(
        steps.map((label, index) => ({
          label,
          status: index === 0 ? "done" : index === 1 ? "active" : "pending",
        })),
      );
      const archive = deterministicZip(next.files);
      next.logs.push({
        at: new Date().toISOString(),
        level: "info",
        message: `Deterministic archive created (${archive.size} bytes).`,
        source: "PACKAGE",
      });
      setDeploySteps(
        steps.map((label, index) => ({
          label,
          status: index < 2 ? "done" : index === 2 ? "active" : "pending",
        })),
      );
      const response = await fetch("/api/admin/workers/install", {
        method: "POST",
        headers: {
          "x-api-key": sessionStorage.getItem("edger.cpanel.apiKey") ?? "",
        },
        body: archive,
      });
      const payload = (await response.json().catch(() => ({}))) as Record<
        string,
        unknown
      >;
      if (!response.ok)
        throw new Error(
          typeof payload.message === "string"
            ? payload.message
            : `Deploy failed (${response.status})`,
        );
      next.name = manifest.name!;
      next.version = manifest.version!;
      next.previewUrl = `/${manifest.name}@${manifest.version}`;
      next.deployments.unshift({
        at: new Date().toISOString(),
        ...payload,
        previewUrl: next.previewUrl,
        status: "Succeeded",
      });
      next.logs.push({
        at: new Date().toISOString(),
        level: "success",
        message: `Deployment completed. Preview now targets ${next.previewUrl}.`,
        source: "DEPLOY",
      });
      next.deployments = next.deployments.slice(0, 50);
      await saveProject(next);
      return next;
    },
    onSuccess: (next) => {
      setProject(next);
      setDeploySteps((steps) =>
        steps.map((step) => ({ ...step, status: "done" })),
      );
      setFooterTab("deployments");
      setFooterVisible(true);
      if (settings.preview.autoPreview) {
        setPreviewVisible(true);
        setRefreshKey((value) => value + 1);
      }
      setMessage(`Deployed ${next.name}@${next.version}`);
    },
    onError: (reason) => {
      setDeploySteps((steps) =>
        steps.map((step) =>
          step.status === "active" ? { ...step, status: "failed" } : step,
        ),
      );
      setMessage(reason instanceof Error ? reason.message : String(reason));
      setFooterTab("logs");
      setFooterVisible(true);
    },
  });

  function selectFile(path: string) {
    setSelected(path);
    setOpenTabs((tabs) => (tabs.includes(path) ? tabs : [...tabs, path]));
  }
  function closeTab(path: string) {
    setOpenTabs((tabs) => {
      const next = tabs.filter((tab) => tab !== path);
      if (selected === path)
        setSelected(
          next.at(Math.max(0, tabs.indexOf(path) - 1)) ?? next[0] ?? "",
        );
      return next;
    });
  }
  function updateFile(value: string) {
    setProject((current) => ({
      ...current,
      files: { ...current.files, [selected]: value },
    }));
  }
  function updateSetting<
    Group extends keyof FullSettings,
    Name extends keyof FullSettings[Group],
  >(
    scope: SettingsScope,
    group: Group,
    name: Name,
    value: FullSettings[Group][Name],
  ) {
    if (scope === "user") {
      setUserSettings((current) => {
        const next = updatePartialSettings(current, group, name, value);
        localStorage.setItem(USER_SETTINGS_KEY, JSON.stringify(next));
        return next;
      });
    } else
      setProject((current) => ({
        ...current,
        settings: updatePartialSettings(
          current.settings as PartialSettings,
          group,
          name,
          value,
        ),
      }));
  }
  function resetSetting<
    Group extends keyof FullSettings,
    Name extends keyof FullSettings[Group],
  >(scope: SettingsScope, group: Group, name: Name) {
    if (scope === "user") {
      setUserSettings((current) => {
        const next = unsetPartialSetting(current, group, name);
        localStorage.setItem(USER_SETTINGS_KEY, JSON.stringify(next));
        return next;
      });
    } else
      setProject((current) => ({
        ...current,
        settings: unsetPartialSetting(
          current.settings as PartialSettings,
          group,
          name,
        ),
      }));
  }
  function openFileDialog(
    kind: NonNullable<FileDialog>["kind"],
    path?: string,
  ) {
    setFileDialog({ kind, path });
    setFileDialogValue(path?.split("/").at(-1) ?? "");
    setFileDialogError("");
  }
  function applyFileDialog() {
    if (!fileDialog) return;
    try {
      const value = fileDialogValue.trim();
      if (!value) throw new Error("A name is required.");
      const oldPath = fileDialog.path ?? "";
      const parent = oldPath.includes("/")
        ? oldPath.slice(0, oldPath.lastIndexOf("/"))
        : "";
      const nextPath =
        fileDialog.kind.startsWith("rename") && parent
          ? `${parent}/${value}`
          : value;
      validateFilePath(nextPath);
      if (fileDialog.kind === "file") {
        if (project.files[nextPath] !== undefined)
          throw new Error("A file with this path already exists.");
        setProject((current) => ({
          ...current,
          files: { ...current.files, [nextPath]: "" },
        }));
        selectFile(nextPath);
      } else if (fileDialog.kind === "folder") {
        if (project.folders.includes(nextPath))
          throw new Error("A folder with this path already exists.");
        setProject((current) => ({
          ...current,
          folders: [...current.folders, nextPath],
        }));
      } else if (fileDialog.kind === "rename-file") {
        if (project.files[nextPath] !== undefined)
          throw new Error("A file with this path already exists.");
        const files = { ...project.files, [nextPath]: project.files[oldPath] };
        delete files[oldPath];
        setProject((current) => ({ ...current, files }));
        setOpenTabs((tabs) =>
          tabs.map((tab) => (tab === oldPath ? nextPath : tab)),
        );
        if (selected === oldPath) setSelected(nextPath);
      } else {
        const files = Object.fromEntries(
          Object.entries(project.files).map(([path, content]) => [
            path === oldPath || path.startsWith(`${oldPath}/`)
              ? `${nextPath}${path.slice(oldPath.length)}`
              : path,
            content,
          ]),
        );
        setProject((current) => ({
          ...current,
          files,
          folders: current.folders.map((folder) =>
            folder === oldPath || folder.startsWith(`${oldPath}/`)
              ? `${nextPath}${folder.slice(oldPath.length)}`
              : folder,
          ),
        }));
      }
      setFileDialog(null);
    } catch (reason) {
      setFileDialogError(
        reason instanceof Error ? reason.message : String(reason),
      );
    }
  }
  function removePath(node: TreeNode) {
    if (node.folder) {
      const files = Object.fromEntries(
        Object.entries(project.files).filter(
          ([path]) => path !== node.path && !path.startsWith(`${node.path}/`),
        ),
      );
      setProject((current) => ({
        ...current,
        files,
        folders: current.folders.filter(
          (folder) =>
            folder !== node.path && !folder.startsWith(`${node.path}/`),
        ),
      }));
      setOpenTabs((tabs) => tabs.filter((path) => files[path] !== undefined));
    } else {
      const files = { ...project.files };
      delete files[node.path];
      setProject((current) => ({ ...current, files }));
      closeTab(node.path);
    }
  }
  function runCommand(event: React.FormEvent) {
    event.preventDefault();
    const command = terminalInput.trim();
    if (!command) return;
    const history = [...terminalHistory, { prompt: true, text: command }];
    const output = (text: string, kind = "") => history.push({ kind, text });
    if (command === "help")
      output(
        "Available: help, validate, deploy, preview, files, status, clear",
      );
    else if (command === "files")
      output(Object.keys(project.files).sort().join("\n"));
    else if (command === "status")
      output(
        `project=${project.name}\nversion=${project.version}\npreview=${project.previewUrl || "not deployed"}`,
      );
    else if (command === "preview")
      output(project.previewUrl || "No successful deployment available.");
    else if (command === "validate") {
      try {
        const value = validateProject(project);
        output(`Valid: ${value.name}@${value.version}`, "success");
      } catch (reason) {
        output(
          reason instanceof Error ? reason.message : String(reason),
          "error",
        );
      }
    } else if (command === "deploy") {
      void deployMutation.mutateAsync();
      output("Starting explicit EdgeR deployment…");
    } else if (command === "clear") history.splice(0);
    else output(`Unknown command: ${command}. Type help.`, "error");
    setTerminalHistory(history.slice(-100));
    setTerminalInput("");
  }
  function syncEditorScroll() {
    if (!editorRef.current) return;
    if (syntaxRef.current) {
      syntaxRef.current.scrollTop = editorRef.current.scrollTop;
      syntaxRef.current.scrollLeft = editorRef.current.scrollLeft;
    }
    if (linesRef.current)
      linesRef.current.scrollTop = editorRef.current.scrollTop;
  }

  const renderTree = (nodes: TreeNode[], depth = 0): React.ReactNode =>
    nodes.map((node) => {
      const isCollapsed = collapsed.has(node.path);
      return (
        <React.Fragment key={node.path}>
          <div
            className="group flex h-7 items-center rounded-md text-sm hover:bg-accent"
            style={{ paddingLeft: `${depth * 12 + 4}px` }}
          >
            <button
              className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
              onClick={() =>
                node.folder
                  ? setCollapsed((current) => {
                      const next = new Set(current);
                      next.has(node.path)
                        ? next.delete(node.path)
                        : next.add(node.path);
                      return next;
                    })
                  : selectFile(node.path)
              }
              type="button"
            >
              {node.folder ? (
                <>
                  {isCollapsed ? (
                    <ChevronRightIcon className="size-3.5" />
                  ) : (
                    <ChevronDownIcon className="size-3.5" />
                  )}{" "}
                  {isCollapsed ? (
                    <FolderIcon className="size-4 text-primary" />
                  ) : (
                    <FolderOpenIcon className="size-4 text-primary" />
                  )}
                </>
              ) : (
                <>
                  <span className="w-3.5" />
                  <FileTypeIcon path={node.path} />
                </>
              )}
              <span className="truncate">{node.name}</span>
            </button>
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <Button
                    aria-label={`Actions for ${node.name}`}
                    className="opacity-0 group-hover:opacity-100"
                    size="icon-xs"
                    variant="ghost"
                  />
                }
              >
                <MoreVerticalIcon />
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem
                  onClick={() =>
                    openFileDialog(
                      node.folder ? "rename-folder" : "rename-file",
                      node.path,
                    )
                  }
                >
                  Rename…
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuItem
                  variant="destructive"
                  onClick={() => removePath(node)}
                >
                  <Trash2Icon /> Delete
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
          {node.folder && !isCollapsed && renderTree(node.children, depth + 1)}
        </React.Fragment>
      );
    });

  const footerCounts: Record<FooterTab, number> = {
    problems: diagnostics.length,
    logs: project.logs.length,
    terminal: terminalHistory.length,
    deployments: project.deployments.length,
  };
  return (
    <main className="grid h-screen min-h-0 grid-rows-[2.5rem_minmax(0,1fr)] overflow-hidden bg-background text-foreground">
      <header className="flex items-center gap-3 border-b bg-card pr-2">
        <div className="flex h-full w-12 shrink-0 items-center border-r pl-2">
          <ActionButton label="Dashboard" onClick={onHome}>
            <WebhookIcon className="size-5 text-primary dark:text-white" />
          </ActionButton>
        </div>
        <div className="flex min-w-0 items-baseline gap-2">
          <strong className="truncate text-sm">{project.name}</strong>
          <span className="shrink-0 text-xs text-muted-foreground">
            {project.type} · {project.version}
          </span>
        </div>
        <span className="ml-auto truncate text-xs text-muted-foreground">
          {message}
        </span>
        <ActionButton
          label={previewVisible ? "Hide preview" : "Show preview"}
          pressed={previewVisible}
          onClick={() => {
            updateSetting(
              "user",
              "workbench",
              "previewVisible",
              !previewVisible,
            );
          }}
        >
          <EyeIcon />
        </ActionButton>
        <ActionButton
          label={footerVisible ? "Hide panel" : "Show panel"}
          pressed={footerVisible}
          onClick={() => {
            updateSetting(
              "user",
              "workbench",
              "panelVisible",
              !footerVisible,
            );
          }}
        >
          <PanelBottomIcon />
        </ActionButton>
        <ActionButton
          label="Validate project"
          onClick={() => {
            try {
              validateProject(project);
              setMessage("Validation passed");
            } catch (reason) {
              setMessage(
                reason instanceof Error ? reason.message : String(reason),
              );
            }
          }}
        >
          <CheckIcon />
        </ActionButton>
        <ActionButton
          label="Deploy project"
          disabled={deployMutation.isPending}
          onClick={() => deployMutation.mutate()}
        >
          <UploadIcon />
        </ActionButton>
      </header>

      <div className="grid min-h-0 grid-cols-[3rem_14rem_minmax(0,1fr)]">
        <aside className="flex min-h-0 flex-col items-center border-r bg-sidebar py-1 text-sidebar-foreground">
          <ActionButton
            label="Explorer"
            pressed={sidebar === "files"}
            onClick={() => setSidebar("files")}
          >
            <FileIcon className="size-5" />
          </ActionButton>
          <ActionButton
            label="Search"
            pressed={sidebar === "search"}
            onClick={() => setSidebar("search")}
          >
            <SearchIcon className="size-5" />
          </ActionButton>
          <div className="mt-auto">
            <ActionButton
              label="Settings"
              onClick={() => setSettingsOpen(true)}
            >
              <SettingsIcon className="size-5" />
            </ActionButton>
          </div>
        </aside>

        <ScrollArea
          className="min-h-0 border-r bg-sidebar text-sidebar-foreground"
          viewportClassName="p-2"
        >
          {sidebar === "files" ? (
            <>
              <div className="mb-2 flex h-7 items-center gap-1 px-1">
                <strong className="text-xs font-medium">EXPLORER</strong>
                <span className="ml-auto" />
                <ActionButton
                  label="New file"
                  onClick={() => openFileDialog("file")}
                >
                  <FilePlusIcon />
                </ActionButton>
                <ActionButton
                  label="New folder"
                  onClick={() => openFileDialog("folder")}
                >
                  <FolderPlusIcon />
                </ActionButton>
              </div>
              {renderTree(tree)}
            </>
          ) : (
            <>
              <strong className="mb-2 block px-1 text-xs font-medium">
                SEARCH
              </strong>
              <InputGroup>
                <InputGroupInput
                  aria-label="Search project files"
                  value={search}
                  onChange={(event) => setSearch(event.currentTarget.value)}
                />
                <InputGroupButton
                  aria-label="Match case"
                  className={caseSensitive ? "bg-accent" : ""}
                  onClick={() => setCaseSensitive((value) => !value)}
                >
                  Aa
                </InputGroupButton>
                <InputGroupButton
                  aria-label="Use regular expression"
                  className={regex ? "bg-accent" : ""}
                  onClick={() => setRegex((value) => !value)}
                >
                  .*
                </InputGroupButton>
              </InputGroup>
              {searchResult.error && (
                <p className="mt-2 text-xs text-destructive">
                  {searchResult.error}
                </p>
              )}
              <div className="mt-2 flex flex-col gap-2">
                {searchResult.results.map((result) => (
                  <div key={result.path}>
                    <button
                      className="w-full truncate text-left text-xs font-medium"
                      onClick={() => selectFile(result.path)}
                      type="button"
                    >
                      {result.path}
                    </button>
                    {result.matches.slice(0, 20).map((match) => (
                      <button
                        className="block w-full truncate rounded px-2 py-1 text-left font-mono text-xs text-muted-foreground hover:bg-accent"
                        key={`${match.line}-${match.start}`}
                        onClick={() => selectFile(result.path)}
                        type="button"
                      >
                        <span className="mr-2 text-primary">{match.line}</span>
                        {match.text.trim()}
                      </button>
                    ))}
                  </div>
                ))}
              </div>
            </>
          )}
        </ScrollArea>

        <section
          className={`grid min-h-0 ${footerVisible ? "grid-rows-[minmax(0,1fr)_16rem]" : "grid-rows-[minmax(0,1fr)_0]"}`}
        >
          <div
            className={`grid min-h-0 ${previewVisible ? "grid-cols-[minmax(0,3fr)_minmax(18rem,2fr)]" : "grid-cols-1"}`}
          >
            <section className="grid min-h-0 grid-rows-[2.5rem_minmax(0,1fr)_1.5rem] overflow-hidden bg-[#111216] text-[#e7e7ea]">
              <Sortable
                orientation="horizontal"
                value={openTabs}
                onValueChange={(tabs) => setOpenTabs(tabs)}
              >
                <ScrollArea
                  className="h-10 bg-[#18191f]"
                  scrollbars="horizontal"
                >
                  <SortableContent className="flex h-10 w-max min-w-full">
                    {openTabs.map((path) => (
                      <SortableItem
                        className={cn(
                          "flex h-10 min-w-32 max-w-48 items-center border-r border-white/10",
                          path === selected
                            ? "border-t-[3px] border-t-primary bg-[#111216]"
                            : "border-b border-b-white/10 text-white/60",
                        )}
                        key={path}
                        value={path}
                      >
                        <SortableItemHandle asChild>
                          <button
                            className="flex min-w-0 flex-1 items-center gap-2 px-3"
                            onClick={() => setSelected(path)}
                            title={path}
                            type="button"
                          >
                            <FileTypeIcon path={path} />
                            <span className="truncate">
                              {path.split("/").at(-1)}
                            </span>
                          </button>
                        </SortableItemHandle>
                        <Button
                          aria-label={`Close ${path}`}
                          className="text-white/40 hover:bg-white/10 hover:text-white/70"
                          size="icon-xs"
                          variant="ghost"
                          onClick={(event) => {
                            event.stopPropagation();
                            closeTab(path);
                          }}
                        >
                          <XIcon />
                        </Button>
                      </SortableItem>
                    ))}
                    <div
                      aria-hidden
                      className="h-10 min-w-0 flex-1 border-b border-white/10"
                    />
                  </SortableContent>
                </ScrollArea>
              </Sortable>
              <div
                className={`relative grid min-h-0 overflow-hidden font-mono ${settings.editor.wordWrap ? "grid-cols-1" : "grid-cols-[3rem_minmax(0,1fr)]"}`}
                style={{
                  fontFamily: "var(--editor-font-family)",
                  fontSize: "var(--editor-font-size)",
                  lineHeight: 1.55,
                }}
              >
                {settings.editor.lineNumbers && !settings.editor.wordWrap && (
                  <div
                    className="overflow-hidden border-r border-white/10 bg-[#0d0e11] py-2 text-right text-white/35"
                    ref={linesRef}
                  >
                    {source.split("\n").map((_line, index) => (
                      <span className="block pr-3" key={index}>
                        {index + 1}
                      </span>
                    ))}
                  </div>
                )}
                <div className="relative min-h-0 overflow-hidden">
                  <pre
                    aria-hidden
                    className={`pointer-events-none absolute inset-0 overflow-hidden whitespace-pre p-2 ${settings.editor.wordWrap ? "whitespace-pre-wrap break-all" : ""}`}
                    dangerouslySetInnerHTML={{
                      __html: highlightCode(selected, source),
                    }}
                    ref={syntaxRef}
                  />
                  <textarea
                    aria-label={`${selected} editor`}
                    className={`absolute inset-0 size-full resize-none overflow-auto whitespace-pre border-0 bg-transparent p-2 text-transparent caret-white outline-none selection:bg-primary/40 ${settings.editor.wordWrap ? "whitespace-pre-wrap break-all" : ""}`}
                    onChange={(event) => updateFile(event.currentTarget.value)}
                    onScroll={syncEditorScroll}
                    onKeyDown={(event) => {
                      if (event.key === "Tab") {
                        event.preventDefault();
                        const target = event.currentTarget;
                        const start = target.selectionStart;
                        const value = `${target.value.slice(0, start)}${" ".repeat(settings.editor.tabSize)}${target.value.slice(target.selectionEnd)}`;
                        updateFile(value);
                        requestAnimationFrame(() => {
                          target.selectionStart = target.selectionEnd =
                            start + settings.editor.tabSize;
                        });
                      }
                    }}
                    ref={editorRef}
                    spellCheck={false}
                    value={source}
                  />
                </div>
              </div>
              <footer className="flex items-center bg-primary px-3 text-[11px] text-primary-foreground">
                <button
                  onClick={() => {
                    setFooterTab("problems");
                    setFooterVisible(true);
                  }}
                  type="button"
                >
                  {diagnostics.length
                    ? `${diagnostics.length} problem${diagnostics.length === 1 ? "" : "s"}`
                    : "No problems"}
                </button>
                <span className="ml-auto">
                  {languageFor(selected)} · Spaces: {settings.editor.tabSize} ·
                  UTF-8
                </span>
              </footer>
            </section>

            {previewVisible && (
              <aside className="grid min-h-0 grid-rows-[2.5rem_minmax(0,1fr)] border-l bg-white text-black">
                <header className="flex items-center gap-2 border-b px-3 text-sm">
                  <strong>Preview</strong>
                  <span className="ml-auto" />
                  <ActionButton
                    label="Refresh preview"
                    disabled={!project.previewUrl}
                    onClick={() => setRefreshKey((value) => value + 1)}
                  >
                    <RefreshCwIcon />
                  </ActionButton>
                  <ActionButton
                    label="Open preview"
                    disabled={!project.previewUrl}
                    onClick={() =>
                      project.previewUrl &&
                      window.open(
                        project.previewUrl,
                        "_blank",
                        "noopener,noreferrer",
                      )
                    }
                  >
                    <ExternalLinkIcon />
                  </ActionButton>
                </header>
                {project.previewUrl ? (
                  <iframe
                    className="size-full border-0"
                    key={refreshKey}
                    src={project.previewUrl}
                    title="Deployed project preview"
                  />
                ) : (
                  <div className="grid place-items-center bg-muted/30 p-8 text-center">
                    <div>
                      <EyeIcon className="mx-auto mb-3 size-6 text-muted-foreground" />
                      <h3 className="font-heading font-medium">
                        No deployment to preview
                      </h3>
                      <p className="mt-1 max-w-sm text-sm text-muted-foreground">
                        Autosave stores only the draft. Deploy explicitly to
                        update this panel.
                      </p>
                      <Button
                        className="mt-4"
                        onClick={() => deployMutation.mutate()}
                      >
                        Deploy project
                      </Button>
                    </div>
                  </div>
                )}
              </aside>
            )}
          </div>

          <section className="min-h-0 overflow-hidden border-t bg-card">
            <div className="flex h-9 items-center border-b px-2">
              <Sortable
                orientation="horizontal"
                value={footerOrder}
                onValueChange={(tabs) => {
                  setFooterOrder(tabs);
                  localStorage.setItem(
                    "edger.webide.footerOrder",
                    JSON.stringify(tabs),
                  );
                }}
              >
                <SortableContent className="flex h-full items-center">
                  {footerOrder.map((tab) => (
                    <SortableItem className="h-full" key={tab} value={tab}>
                      <SortableItemHandle asChild>
                        <Button
                          className={cn(
                            "h-full rounded-none border-0 border-b-2 bg-transparent shadow-none hover:bg-transparent",
                            footerTab === tab
                              ? "border-primary text-foreground"
                              : "border-transparent text-muted-foreground",
                          )}
                          onClick={() => setFooterTab(tab)}
                          variant="ghost"
                        >
                          {tab === "problems" ? (
                            <CircleAlertIcon />
                          ) : tab === "terminal" ? (
                            <TerminalIcon />
                          ) : tab === "deployments" ? (
                            <UploadIcon />
                          ) : (
                            <ListIcon />
                          )}
                          <span className="capitalize">{tab}</span>
                          {footerCounts[tab] > 0 && (
                            <Badge variant="secondary">
                              {footerCounts[tab]}
                            </Badge>
                          )}
                        </Button>
                      </SortableItemHandle>
                    </SortableItem>
                  ))}
                </SortableContent>
              </Sortable>
              <span className="ml-auto" />
              <Label className="mr-2 flex items-center gap-2 text-xs text-muted-foreground">
                <Switch
                  checked={settings.logs.preserveAcrossRestarts}
                  onCheckedChange={(checked) =>
                    updateSetting(
                      "workspace",
                      "logs",
                      "preserveAcrossRestarts",
                      checked,
                    )
                  }
                />
                Preserve logs across restarts
              </Label>
            </div>
            <ScrollArea
              className="h-[calc(100%-2.25rem)] font-mono text-xs"
              viewportClassName="p-3"
            >
              {footerTab === "logs" &&
                project.logs.map((log, index) => (
                  <div
                    className="grid grid-cols-[5rem_6rem_minmax(0,1fr)] gap-3 py-1"
                    key={`${log.at}-${index}`}
                  >
                    <time className="text-muted-foreground">
                      {new Date(log.at).toLocaleTimeString()}
                    </time>
                    <span className="text-primary">{log.source}</span>
                    <span>{log.message}</span>
                  </div>
                ))}
              {footerTab === "problems" &&
                (diagnostics.length ? (
                  diagnostics.map((diagnostic, index) => (
                    <button
                      className="flex w-full items-start gap-3 rounded p-2 text-left hover:bg-accent"
                      key={`${diagnostic.line}-${index}`}
                      onClick={() => {
                        editorRef.current?.focus();
                      }}
                      type="button"
                    >
                      <CircleAlertIcon className="size-4 text-destructive" />
                      <span>
                        Ln {diagnostic.line}: {diagnostic.message}
                      </span>
                    </button>
                  ))
                ) : (
                  <p className="text-muted-foreground">
                    No problems detected in {selected}.
                  </p>
                ))}
              {footerTab === "deployments" && (
                <div className="flex flex-col gap-2">
                  {deploySteps.length > 0 && (
                    <div className="mb-2 flex flex-wrap gap-2">
                      {deploySteps.map((step) => (
                        <Badge
                          key={step.label}
                          variant={
                            step.status === "failed"
                              ? "destructive"
                              : step.status === "done"
                                ? "secondary"
                                : "outline"
                          }
                        >
                          {step.label}: {step.status}
                        </Badge>
                      ))}
                    </div>
                  )}
                  {project.deployments.map((deployment, index) => (
                    <div
                      className="rounded border p-2"
                      key={`${deployment.at}-${index}`}
                    >
                      <strong>{deployment.status}</strong>
                      <span className="ml-3 text-muted-foreground">
                        {new Date(deployment.at).toLocaleString()}
                      </span>
                    </div>
                  ))}
                  {project.deployments.length === 0 && (
                    <p className="text-muted-foreground">No deployments yet.</p>
                  )}
                </div>
              )}
              {footerTab === "terminal" && (
                <div className="flex h-full flex-col">
                  <div className="flex-1 whitespace-pre-wrap">
                    {terminalHistory.map((entry, index) => (
                      <div
                        className={
                          entry.kind === "error"
                            ? "text-destructive"
                            : entry.kind === "success"
                              ? "text-primary"
                              : ""
                        }
                        key={index}
                      >
                        {entry.prompt ? "> " : ""}
                        {entry.text}
                      </div>
                    ))}
                  </div>
                  <form
                    className="flex items-center gap-2 border-t pt-2"
                    onSubmit={runCommand}
                  >
                    <span>&gt;</span>
                    <Input
                      className="h-7 border-0 font-mono shadow-none"
                      aria-label="Terminal command"
                      value={terminalInput}
                      onChange={(event) =>
                        setTerminalInput(event.currentTarget.value)
                      }
                    />
                  </form>
                </div>
              )}
            </ScrollArea>
          </section>
        </section>
      </div>

      <SettingsDialog
        onOpenChange={setSettingsOpen}
        onReset={resetSetting}
        onSet={updateSetting}
        open={settingsOpen}
        snapshot={snapshot}
      />
      <Dialog
        open={Boolean(fileDialog)}
        onOpenChange={(open) => {
          if (!open) setFileDialog(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {fileDialog?.kind.includes("rename")
                ? "Rename"
                : fileDialog?.kind === "folder"
                  ? "New folder"
                  : "New file"}
            </DialogTitle>
            <DialogDescription>
              Paths are relative to the project root.
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="file-path">Name or path</Label>
            <Input
              autoFocus
              id="file-path"
              value={fileDialogValue}
              onChange={(event) =>
                setFileDialogValue(event.currentTarget.value)
              }
              onKeyDown={(event) => {
                if (event.key === "Enter") applyFileDialog();
              }}
            />
          </div>
          {fileDialogError && (
            <p className="text-sm text-destructive">{fileDialogError}</p>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setFileDialog(null)}>
              Cancel
            </Button>
            <Button onClick={applyFileDialog}>Apply</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </main>
  );
}
