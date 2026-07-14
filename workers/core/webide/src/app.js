import { highlightCode, languageFor, lintDocument, reorderItems, searchProjectFiles } from "./editor-tools.js";
import { icon } from "./icons.js";
import { getIcon as getMaterialFileIcon } from "./vendor/material-file-icons.js";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  Badge,
  Button,
  ButtonLink,
  CardContent,
  CardDescription,
  CardTitle,
  Checkbox,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Empty,
  EmptyDescription,
  EmptyTitle,
  Field,
  FieldError,
  FieldLabel,
  Input,
  InputGroup,
  InputGroupAddon,
  ResizableHandle,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TabsContent,
  TabsList,
  TabsTrigger,
  Textarea,
  Toast,
  Toaster,
  Tooltip,
} from "./components/ui/index.js";

const SESSION_KEY = "edger.cpanel.apiKey";
const DB_NAME = "edger-webide";
const DB_VERSION = 2;
const PROJECT_STORE = "projects";
const LEGACY_STORE = "drafts";
const FOOTER_TABS = ["problems", "logs", "terminal", "deployments"];
const DEPLOY_STEPS = [
  "Validation",
  "Packaging",
  "Upload",
  "Release / migrations",
  "Health check",
  "Activation",
  "Complete",
];

const templates = {
  FetchHandler: {
    category: "backend",
    description: "Request handler with a persistent Deno process.",
    icon: "code",
    name: "Fetch Handler",
    runtime: "EdgeR · Deno",
    supported: true,
    files: {
      "manifest.yaml": 'name: hello-webide\nversion: "1.0.0"\nentrypoint: index.ts\nkind: fetch\n',
      "index.ts": 'export default {\n  fetch() {\n    return new Response("Hello from EdgeR");\n  },\n};\n',
    },
  },
  RoutesTable: {
    category: "backend",
    description: "Declarative routes with params and method maps.",
    icon: "route",
    name: "Routes Table",
    runtime: "EdgeR · Deno",
    supported: true,
    files: {
      "manifest.yaml": 'name: routes-webide\nversion: "1.0.0"\nentrypoint: index.ts\nkind: routes\n',
      "index.ts": 'export const routes = {\n  "/": () => new Response("Home"),\n  "/hello/:name": ({ params }) => new Response(`Hello ${params.name}`),\n};\n',
    },
  },
  StaticSpa: {
    category: "frontend",
    description: "Static application served directly by EdgeR.",
    icon: "browser",
    name: "Static SPA",
    runtime: "EdgeR · Static",
    supported: true,
    files: {
      "manifest.yaml": 'name: static-webide\nversion: "1.0.0"\nentrypoint: index.html\n',
      "index.html": '<!doctype html>\n<html lang="en">\n  <head><meta charset="utf-8"><title>EdgeR app</title></head>\n  <body><h1>Hello from EdgeR</h1></body>\n</html>\n',
    },
  },
  React: {
    category: "frontend",
    description: "React 19 SPA using browser-native ESM imports.",
    icon: "react",
    name: "React",
    runtime: "Static SPA · ESM",
    supported: true,
    files: {
      "manifest.yaml": 'name: react-webide\nversion: "1.0.0"\nentrypoint: index.html\n',
      "index.html": '<!doctype html>\n<html lang="en">\n  <head>\n    <meta charset="utf-8">\n    <meta name="viewport" content="width=device-width,initial-scale=1">\n    <title>React on EdgeR</title>\n    <script type="importmap">{"imports":{"react":"https://esm.sh/react@19","react-dom/client":"https://esm.sh/react-dom@19/client"}}</script>\n  </head>\n  <body><div id="root"></div><script type="module" src="./app.js"></script></body>\n</html>\n',
      "app.js": 'import React from "react";\nimport { createRoot } from "react-dom/client";\n\nfunction App() {\n  const [count, setCount] = React.useState(0);\n  return React.createElement("main", null,\n    React.createElement("h1", null, "React on EdgeR"),\n    React.createElement("button", { onClick: () => setCount(count + 1) }, `Count: ${count}`),\n  );\n}\n\ncreateRoot(document.getElementById("root")).render(React.createElement(App));\n',
    },
  },
  Vue: {
    category: "frontend",
    description: "Vue 3 SPA using the browser ESM build.",
    icon: "vue",
    name: "Vue",
    runtime: "Static SPA · ESM",
    supported: true,
    files: {
      "manifest.yaml": 'name: vue-webide\nversion: "1.0.0"\nentrypoint: index.html\n',
      "index.html": '<!doctype html>\n<html lang="en">\n  <head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Vue on EdgeR</title></head>\n  <body><div id="app"></div><script type="module" src="./app.js"></script></body>\n</html>\n',
      "app.js": 'import { createApp, ref } from "https://esm.sh/vue@3/dist/vue.esm-browser.prod.js";\n\ncreateApp({\n  setup() {\n    const count = ref(0);\n    return { count };\n  },\n  template: `<main><h1>Vue on EdgeR</h1><button @click="count++">Count: {{ count }}</button></main>`,\n}).mount("#app");\n',
    },
  },
  Svelte: {
    category: "frontend",
    description: "Svelte project with a compile step.",
    icon: "svelte",
    name: "Svelte",
    runtime: "Build pipeline required",
    supported: false,
  },
  TanStackStart: {
    category: "fullstack",
    description: "Full-stack React application powered by TanStack Start.",
    icon: "tanstack",
    name: "TanStack Start",
    runtime: "Fullstack pipeline required",
    supported: false,
  },
  NextJs: {
    category: "fullstack",
    description: "Next.js application with server rendering and routing.",
    icon: "nextjs",
    name: "Next.js",
    runtime: "Node-compatible pipeline required",
    supported: false,
  },
};

const templateCategories = [
  { id: "frontend", label: "Frontend" },
  { id: "backend", label: "Backend" },
  { id: "fullstack", label: "Fullstack" },
];

const state = {
  screen: "dashboard",
  dashboardSection: "dashboard",
  projects: [],
  activeProjectId: null,
  query: "",
  selected: "",
  openTabs: [],
  dirty: false,
  saving: false,
  deploying: false,
  deploySteps: [],
  footerTab: "logs",
  footerOrder: sanitizeFooterOrder(loadJsonStorage("edger.webide.footerOrder")),
  footerVisible: localStorage.getItem("edger.webide.footerVisible") !== "false",
  previewVisible: localStorage.getItem("edger.webide.previewVisible") !== "false",
  sidebarView: "files",
  searchQuery: "",
  searchCaseSensitive: false,
  searchRegex: false,
  preserveLogs: localStorage.getItem("edger.webide.preserveLogs") === "true",
  collapsedFolders: new Set(),
  fileDialog: null,
  projectDialog: null,
  toast: null,
  terminalHistory: [],
  message: "Projects are stored locally in this browser.",
  templateCategory: "frontend",
  templateModalOpen: false,
};

function loadJsonStorage(key) {
  try { return JSON.parse(localStorage.getItem(key) || "null"); }
  catch { return null; }
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function fileTypeIcon(path, size = 14) {
  return `<span aria-hidden="true" class="file-type-icon material" style="--icon-size:${size}px">${getMaterialFileIcon(path.split("/").at(-1)).svg}</span>`;
}

function sanitizeFooterOrder(value) {
  const source = Array.isArray(value) ? value : [];
  return [...new Set([...source.filter((item) => FOOTER_TABS.includes(item)), ...FOOTER_TABS])];
}

function makeId() {
  return globalThis.crypto?.randomUUID?.() || `${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function slugify(value) {
  return value.toLowerCase().trim().replace(/[^a-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "") || "edger-app";
}

function parseManifest(text) {
  const value = (key) => text.match(new RegExp(`^${key}:\\s*["']?([^"'\\n]+)`, "m"))?.[1]?.trim();
  return { name: value("name"), version: value("version"), entrypoint: value("entrypoint"), kind: value("kind") };
}

function project() {
  return state.projects.find((item) => item.id === state.activeProjectId) || null;
}

function createProjectRecord(type, requestedName) {
  const definition = templates[type];
  if (!definition?.supported || !definition.files) throw new Error(`${definition?.name || type} is not deployable by EdgeR yet`);
  const name = slugify(requestedName || `${type.toLowerCase()}-app`);
  const files = structuredClone(definition.files);
  files["manifest.yaml"] = files["manifest.yaml"].replace(/^name:.*$/m, `name: ${name}`);
  const selected = Object.keys(files).find((file) => file !== "manifest.yaml") || "manifest.yaml";
  const now = new Date().toISOString();
  return {
    id: makeId(),
    name,
    type,
    version: "1.0.0",
    files,
    folders: [],
    selected,
    createdAt: now,
    updatedAt: now,
    previewUrl: "",
    deployments: [],
    logs: [{ at: now, source: "WEBIDE", level: "info", message: `Created ${name} from ${type}.` }],
  };
}

function validateFilePath(name) {
  if (!name || name.startsWith("/") || name.includes("\\") || name.split("/").some((part) => !part || part === "." || part === "..")) {
    throw new Error(`Invalid project file path: ${name || "(empty)"}`);
  }
}

function validateProject(active = project()) {
  if (!active) throw new Error("Open a project first");
  const manifest = active.files["manifest.yaml"];
  if (!manifest) throw new Error("manifest.yaml is required");
  const parsed = parseManifest(manifest);
  if (!parsed.name || !parsed.version) throw new Error("Manifest name and version are required");
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(parsed.name)) throw new Error("Worker name must be URL-safe");
  if (!parsed.entrypoint || !active.files[parsed.entrypoint]) throw new Error("Manifest entrypoint must exist in the project");
  for (const name of Object.keys(active.files)) validateFilePath(name);
  active.name = parsed.name;
  active.version = parsed.version;
  return parsed;
}

function openDb() {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(PROJECT_STORE)) request.result.createObjectStore(PROJECT_STORE, { keyPath: "id" });
      if (!request.result.objectStoreNames.contains(LEGACY_STORE)) request.result.createObjectStore(LEGACY_STORE);
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

async function loadProjects() {
  const db = await openDb();
  const projects = await new Promise((resolve, reject) => {
    const request = db.transaction(PROJECT_STORE).objectStore(PROJECT_STORE).getAll();
    request.onsuccess = () => resolve(request.result || []);
    request.onerror = () => reject(request.error);
  });
  if (projects.length) return projects.map((item) => ({ ...item, folders: item.folders || [] })).sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));

  const legacy = await new Promise((resolve) => {
    const request = db.transaction(LEGACY_STORE).objectStore(LEGACY_STORE).get("current");
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => resolve(null);
  });
  if (!legacy?.files) return [];
  const manifest = parseManifest(legacy.files["manifest.yaml"] || "");
  const migrated = createProjectRecord(legacy.type || "FetchHandler", manifest.name || "hello-webide");
  migrated.files = legacy.files;
  migrated.selected = legacy.selected || migrated.selected;
  migrated.version = manifest.version || "1.0.0";
  await saveProject(migrated);
  return [migrated];
}

async function saveProject(active) {
  const db = await openDb();
  active.updatedAt = new Date().toISOString();
  await new Promise((resolve, reject) => {
    const tx = db.transaction(PROJECT_STORE, "readwrite");
    tx.objectStore(PROJECT_STORE).put(structuredClone(active));
    tx.oncomplete = resolve;
    tx.onerror = () => reject(tx.error);
  });
}

async function removeProject(id) {
  const db = await openDb();
  await new Promise((resolve, reject) => {
    const tx = db.transaction(PROJECT_STORE, "readwrite");
    tx.objectStore(PROJECT_STORE).delete(id);
    tx.oncomplete = resolve;
    tx.onerror = () => reject(tx.error);
  });
}

let saveTimer;
let suppressReorderClick = false;
function scheduleSave() {
  const active = project();
  if (!active) return;
  state.dirty = true;
  state.message = "Unsaved local changes";
  renderStatus();
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    state.saving = true;
    renderStatus();
    try {
      await saveProject(active);
      state.dirty = false;
      state.message = "Draft saved locally";
      sortProjects();
    } catch (error) {
      state.message = error.message;
    } finally {
      state.saving = false;
      renderStatus();
    }
  }, 350);
}

function workbenchStateKey(id) {
  return `edger.webide.workbench.${id}`;
}

function persistWorkbenchState(active) {
  localStorage.setItem(workbenchStateKey(active.id), JSON.stringify({
    selected: active.selected,
    openTabs: active.openTabs,
  }));
  void saveProject(active).catch((error) => {
    state.message = error.message;
    renderStatus();
  });
}

function sortProjects() {
  state.projects.sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
}

function appendLog(message, level = "info", source = "WEBIDE") {
  const active = project();
  if (!active) return;
  active.logs ||= [];
  active.logs.push({ at: new Date().toISOString(), source, level, message });
  active.logs = active.logs.slice(-200);
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let i = 0; i < 8; i += 1) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function u16(value) { return [value & 255, (value >>> 8) & 255]; }
function u32(value) { return [...u16(value), ...u16(value >>> 16)]; }

function deterministicZip(files) {
  const encoder = new TextEncoder();
  const entries = Object.entries(files).sort(([a], [b]) => a.localeCompare(b));
  const local = [];
  const central = [];
  let offset = 0;
  for (const [name, content] of entries) {
    const filename = encoder.encode(name);
    const data = encoder.encode(content);
    const crc = crc32(data);
    const header = [
      ...u32(0x04034b50), ...u16(20), ...u16(0x0800), ...u16(0), ...u16(0), ...u16(33),
      ...u32(crc), ...u32(data.length), ...u32(data.length), ...u16(filename.length), ...u16(0), ...filename,
    ];
    local.push(...header, ...data);
    central.push(
      ...u32(0x02014b50), ...u16(20), ...u16(20), ...u16(0x0800), ...u16(0), ...u16(0), ...u16(33),
      ...u32(crc), ...u32(data.length), ...u32(data.length), ...u16(filename.length), ...u16(0),
      ...u16(0), ...u16(0), ...u16(0), ...u32(0), ...u32(offset), ...filename,
    );
    offset += header.length + data.length;
  }
  const end = [
    ...u32(0x06054b50), ...u16(0), ...u16(0), ...u16(entries.length), ...u16(entries.length),
    ...u32(central.length), ...u32(local.length), ...u16(0),
  ];
  return new Blob([new Uint8Array([...local, ...central, ...end])], { type: "application/zip" });
}

async function deploy() {
  const active = project();
  if (!active || state.deploying) return;
  state.deploying = true;
  state.deploySteps = DEPLOY_STEPS.map((label) => ({ label, status: "pending" }));
  if (!state.preserveLogs) active.logs = [];
  const requested = parseManifest(active.files["manifest.yaml"] || "");
  appendLog(`Deploy requested for ${requested.name || active.name}@${requested.version || active.version}.`, "info", "DEPLOY");
  renderWorkbench();
  const previousPreview = active.previewUrl;
  try {
    markStep(0, "active");
    const manifest = validateProject(active);
    active.version = manifest.version;
    appendLog("Manifest and project files validated.", "info", "VALIDATE");
    markStep(0, "done");
    markStep(1, "active");
    const zip = deterministicZip(active.files);
    appendLog(`Deterministic archive created (${zip.size} bytes).`, "info", "PACKAGE");
    markStep(1, "done");
    markStep(2, "active");
    const response = await fetch("/api/admin/workers/install", {
      method: "POST",
      headers: { "x-api-key": sessionStorage.getItem(SESSION_KEY) || "" },
      body: zip,
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(payload.message || `Deploy failed (${response.status})`);
    markStep(2, "done");
    markStep(3, payload.release === "completed" || payload.release === "not_configured" ? "done" : "failed");
    markStep(4, payload.health === "passed" || payload.health === "not_configured" ? "done" : "failed");
    markStep(5, payload.activation === "active" ? "done" : "failed");
    active.previewUrl = `/${manifest.name}@${manifest.version}`;
    markStep(6, "done");
    active.deployments.unshift({ at: new Date().toISOString(), ...payload, status: "Succeeded", previewUrl: active.previewUrl });
    appendLog(`Deployment completed. Preview now targets ${active.previewUrl}.`, "success", "DEPLOY");
    state.message = `Deployed ${manifest.name}@${manifest.version}`;
  } catch (error) {
    active.previewUrl = previousPreview;
    const current = state.deploySteps.findIndex((step) => step.status === "active");
    if (current >= 0) state.deploySteps[current].status = "failed";
    active.deployments.unshift({ at: new Date().toISOString(), status: "Failed", message: error.message });
    appendLog(error.message, "error", "DEPLOY");
    state.message = error.message;
  } finally {
    state.deploying = false;
    active.deployments = active.deployments.slice(0, 50);
    await saveProject(active).catch(() => {});
    renderWorkbench();
  }
}

function markStep(index, status) {
  state.deploySteps[index].status = status;
  renderDeployProgress();
}

function openProject(id) {
  const active = state.projects.find((item) => item.id === id);
  if (!active) return;
  state.activeProjectId = id;
  state.screen = "workbench";
  const savedWorkbench = loadJsonStorage(workbenchStateKey(id));
  const savedSelected = savedWorkbench?.selected || active.selected;
  const savedTabs = Array.isArray(savedWorkbench?.openTabs) ? savedWorkbench.openTabs : active.openTabs;
  const initialSelected = savedSelected && active.files[savedSelected] !== undefined ? savedSelected : Object.keys(active.files)[0];
  state.openTabs = Array.isArray(savedTabs) ? savedTabs.filter((file) => active.files[file] !== undefined) : [initialSelected];
  state.selected = state.openTabs.includes(initialSelected) ? initialSelected : state.openTabs[0] || "";
  state.message = "Draft ready";
  history.replaceState(null, "", `${location.pathname}?project=${encodeURIComponent(id)}`);
  render();
}

function showDashboard(section = "dashboard") {
  state.screen = "dashboard";
  state.dashboardSection = section;
  state.activeProjectId = null;
  history.replaceState(null, "", location.pathname);
  render();
}

function nextProjectName(type) {
  const label = templates[type]?.name || type.replace(/([a-z])([A-Z])/g, "$1 $2");
  const base = slugify(`${label}-app`);
  const names = new Set(state.projects.map((item) => item.name));
  if (!names.has(base)) return base;
  let suffix = 2;
  while (names.has(`${base}-${suffix}`)) suffix += 1;
  return `${base}-${suffix}`;
}

function createNewProject(type) {
  const next = createProjectRecord(type, nextProjectName(type));
  state.templateModalOpen = false;
  state.projects.unshift(next);
  saveProject(next).then(() => openProject(next.id)).catch(showError);
}

function importedProjectType(manifest) {
  if (manifest.kind === "routes") return "RoutesTable";
  if (manifest.entrypoint?.endsWith(".html")) return "StaticSpa";
  return "FetchHandler";
}

async function importProject(fileList) {
  const selectedFiles = [...fileList];
  if (!selectedFiles.length) return;
  const firstPath = selectedFiles[0].webkitRelativePath || selectedFiles[0].name;
  const root = firstPath.includes("/") ? firstPath.split("/")[0] : "";
  const files = {};
  for (const file of selectedFiles) {
    const relative = file.webkitRelativePath || file.name;
    const path = root && relative.startsWith(`${root}/`) ? relative.slice(root.length + 1) : relative;
    if (!path || path.endsWith("/.DS_Store") || path === ".DS_Store") continue;
    validateFilePath(path);
    files[path] = await file.text();
  }
  const manifest = parseManifest(files["manifest.yaml"] || "");
  if (!manifest.name || !manifest.version || !manifest.entrypoint) {
    throw new Error("Choose a project folder containing a complete manifest.yaml");
  }
  if (!files[manifest.entrypoint]) throw new Error(`Imported entrypoint is missing: ${manifest.entrypoint}`);
  const now = new Date().toISOString();
  const imported = {
    id: makeId(),
    name: slugify(manifest.name),
    type: importedProjectType(manifest),
    version: manifest.version,
    files,
    folders: [...new Set(Object.keys(files).flatMap((path) => {
      const parts = path.split("/");
      return parts.slice(0, -1).map((_, index) => parts.slice(0, index + 1).join("/"));
    }))],
    selected: manifest.entrypoint,
    createdAt: now,
    updatedAt: now,
    previewUrl: "",
    deployments: [],
    logs: [{ at: now, source: "WEBIDE", level: "info", message: `Imported ${manifest.name} from a local folder.` }],
  };
  validateProject(imported);
  state.projects.unshift(imported);
  await saveProject(imported);
  openProject(imported.id);
}

function openTemplateModal(category = "frontend") {
  state.templateCategory = templateCategories.some((item) => item.id === category) ? category : "frontend";
  state.templateModalOpen = true;
  renderDashboard();
  requestAnimationFrame(() => document.querySelector(".template-modal [data-create-template]:not(:disabled)")?.focus());
}

function closeTemplateModal() {
  state.templateModalOpen = false;
  renderDashboard();
  requestAnimationFrame(() => document.querySelector("#new-project")?.focus());
}

function trapDialogFocus(event, selector) {
  if (event.key !== "Tab") return;
  const container = document.querySelector(selector);
  if (!container) return;
  const focusable = [...container.querySelectorAll('button:not(:disabled), a[href], input:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])')]
    .filter((element) => element.offsetWidth > 0 && element.offsetHeight > 0);
  if (!focusable.length) return;
  const first = focusable[0];
  const last = focusable.at(-1);
  if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
  else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
}

function focusSelector(element) {
  if (!element) return "";
  if (element.id) return `#${CSS.escape(element.id)}`;
  for (const name of ["treeMenu", "file", "folder"]) {
    if (element.dataset?.[name]) return `[data-${name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`)}="${CSS.escape(element.dataset[name])}"]`;
  }
  return "";
}

function renderTemplateModal() {
  if (!state.templateModalOpen) return "";
  const options = Object.entries(templates).filter(([, definition]) => definition.category === state.templateCategory);
  const tabs = TabsList({
    className: "template-tabs",
    children: templateCategories.map((category) => TabsTrigger({
      active: category.id === state.templateCategory,
      className: `template-tab ${category.id === state.templateCategory ? "active" : ""}`,
      "data-template-category": category.id,
      children: category.label,
    })).join(""),
  });
  const cards = TabsContent({
    className: "template-options",
    children: options.map(([type, definition]) => Button({
      className: "template-option",
      variant: "ghost",
      "data-create-template": type,
      disabled: !definition.supported,
      title: definition.supported ? definition.description : `${definition.name} requires runtime capabilities that are not available yet.`,
      children: `<span class="template-option-icon">${icon(definition.icon, 22)}</span>${CardContent({ className: "template-option-copy", children: `${CardTitle({ children: escapeHtml(definition.name) })}${CardDescription({ children: escapeHtml(definition.runtime) })}` })}${Badge({ className: `template-availability ${definition.supported ? "ready" : "planned"}`, children: definition.supported ? "Ready" : "Planned" })}`,
    })).join(""),
  });
  return Dialog({
    className: "dialog-overlay",
    "data-close-template-modal": true,
    children: DialogContent({
      as: "section",
      className: "template-modal",
      id: "template-dialog",
      "aria-labelledby": "template-dialog-title",
      children: `${DialogHeader({ className: "template-modal-header", children: `<div>${DialogTitle({ id: "template-dialog-title", children: "Create a new project" })}${DialogDescription({ children: "Choose a starter that matches what you want to deploy on EdgeR." })}</div>${Button({ ariaLabel: undefined, "aria-label": "Close template picker", className: "icon-button", size: "icon-sm", variant: "ghost", id: "close-template-modal", children: icon("close") })}` })}${tabs}${cards}<footer class="template-modal-footer"><p>Planned templates remain disabled until EdgeR can build and run them safely.</p></footer>`,
    }),
  });
}

function duplicateProject(id) {
  const source = state.projects.find((item) => item.id === id);
  if (!source) return;
  const copy = structuredClone(source);
  copy.id = makeId();
  copy.name = `${source.name}-copy`;
  copy.files["manifest.yaml"] = copy.files["manifest.yaml"].replace(/^name:.*$/m, `name: ${copy.name}`);
  copy.previewUrl = "";
  copy.deployments = [];
  copy.logs = [{ at: new Date().toISOString(), source: "WEBIDE", level: "info", message: `Duplicated from ${source.name}.` }];
  copy.createdAt = new Date().toISOString();
  copy.updatedAt = copy.createdAt;
  state.projects.unshift(copy);
  saveProject(copy).then(() => render()).catch(showError);
}

function openProjectDialog(kind, id) {
  const active = state.projects.find((item) => item.id === id);
  if (!active) return;
  state.projectDialog = { kind, projectId: id, value: active.name, error: "" };
  renderDashboard();
  requestAnimationFrame(() => document.querySelector(kind === "rename" ? "#project-dialog-input" : "#cancel-project-dialog")?.focus());
}

function closeProjectDialog() {
  const dialog = state.projectDialog;
  state.projectDialog = null;
  renderDashboard();
  requestAnimationFrame(() => document.querySelector(`[data-${dialog?.kind}="${dialog?.projectId}"]`)?.focus());
}

function renderProjectDialog() {
  const dialog = state.projectDialog;
  if (!dialog) return "";
  const active = state.projects.find((item) => item.id === dialog.projectId);
  if (!active) return "";
  const deleting = dialog.kind === "delete";
  const parts = deleting
    ? { Root: AlertDialog, Content: AlertDialogContent, Header: AlertDialogHeader, Title: AlertDialogTitle, Description: AlertDialogDescription, Footer: AlertDialogFooter }
    : { Root: Dialog, Content: DialogContent, Header: DialogHeader, Title: DialogTitle, Description: DialogDescription, Footer: DialogFooter };
  const header = parts.Header({
    children: `${parts.Title({ id: "project-dialog-title", children: deleting ? "Delete project" : "Rename project" })}${deleting ? parts.Description({ children: `Delete local project <strong>${escapeHtml(active.name)}</strong>? Only the local draft will be removed. Deployed workers are not removed.` }) : ""}`,
  });
  const field = deleting ? "" : Field({
    children: FieldLabel({
      htmlFor: "project-dialog-input",
      children: `Project name${Input({ id: "project-dialog-input", autocomplete: "off", value: dialog.value, "aria-invalid": Boolean(dialog.error) })}`,
    }),
  });
  const error = dialog.error ? FieldError({ className: "dialog-error", children: escapeHtml(dialog.error) }) : "";
  const footer = parts.Footer({
    children: `${Button({ className: "button", variant: "outline", id: "cancel-project-dialog", children: "Cancel" })}${Button({ className: `button ${deleting ? "danger" : "primary"}`, variant: deleting ? "destructive" : "default", type: "submit", children: deleting ? "Delete" : "Rename" })}`,
  });
  return parts.Root({
    className: "workbench-dialog-overlay",
    id: "project-dialog-overlay",
    children: parts.Content({ className: "workbench-dialog", id: "project-dialog", "aria-labelledby": "project-dialog-title", children: `${header}${field}${error}${footer}` }),
  });
}

let toastTimer;
function renderToaster() {
  const toast = state.toast;
  return Toaster({
    className: "toaster",
    children: toast ? Toast({
      className: `toast ${escapeHtml(toast.variant)}`,
      variant: toast.variant,
      title: escapeHtml(toast.title),
      description: escapeHtml(toast.description),
      children: Button({ className: "icon-button", variant: "ghost", size: "icon-sm", id: "dismiss-toast", "aria-label": "Dismiss notification", children: icon("close", 14) }),
    }) : "",
  });
}

function syncToaster() {
  const toaster = document.querySelector('[data-slot="toaster"]');
  if (!toaster) return;
  toaster.outerHTML = renderToaster();
  document.querySelector("#dismiss-toast")?.addEventListener("click", () => {
    clearTimeout(toastTimer);
    state.toast = null;
    syncToaster();
  });
}

function showToast(title, description = "", variant = "default") {
  state.toast = { title, description, variant };
  syncToaster();
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { state.toast = null; syncToaster(); }, 4500);
}

async function applyProjectDialog() {
  const dialog = state.projectDialog;
  const active = state.projects.find((item) => item.id === dialog?.projectId);
  if (!dialog || !active) return;
  if (dialog.kind === "rename") {
    if (!dialog.value.trim()) throw new Error("Project name is required.");
    active.name = slugify(dialog.value);
    active.files["manifest.yaml"] = active.files["manifest.yaml"].replace(/^name:.*$/m, `name: ${active.name}`);
    await saveProject(active);
    sortProjects();
  } else {
    await removeProject(active.id);
    localStorage.removeItem(workbenchStateKey(active.id));
    state.projects = state.projects.filter((item) => item.id !== active.id);
  }
  state.projectDialog = null;
  renderDashboard();
}

function render() {
  if (state.screen === "dashboard") renderDashboard();
  else renderWorkbench();
}

function renderProjectTable(items) {
  const header = TableHeader({ children: TableRow({
    className: "project-row project-head",
    children: ["Project", "Runtime", "Version", "Updated", ""].map((label) => TableHead({ children: label })).join(""),
  }) });
  const body = TableBody({
    children: items.map((item) => {
      const definition = templates[item.type] || templates.FetchHandler;
      const name = TableCell({ children: `<div class="project-name"><span class="project-avatar">${icon(definition.icon, 18)}</span><span><strong>${escapeHtml(item.name)}</strong><small>${item.previewUrl ? `Deployed at ${escapeHtml(item.previewUrl)}` : "Local draft"}</small></span></div>` });
      const runtime = TableCell({ children: escapeHtml(item.type.replace(/([a-z])([A-Z])/g, "$1 $2")) });
      const version = TableCell({ children: `<code>${escapeHtml(item.version)}</code>` });
      const updated = TableCell({ children: `<time>${escapeHtml(new Date(item.updatedAt).toLocaleString([], { dateStyle: "medium", timeStyle: "short" }))}</time>` });
      const actions = TableCell({
        children: `<span class="row-actions">${Button({ className: "icon-button", size: "icon-sm", variant: "ghost", title: "Duplicate project", "aria-label": `Duplicate ${item.name}`, "data-duplicate": item.id, children: icon("copy") })}${Button({ className: "icon-button", size: "icon-sm", variant: "ghost", title: "Rename project", "aria-label": `Rename ${item.name}`, "data-rename": item.id, children: icon("edit") })}${Button({ className: "icon-button danger", size: "icon-sm", variant: "ghost", title: "Delete project", "aria-label": `Delete ${item.name}`, "data-delete": item.id, children: icon("trash") })}</span>`,
      });
      return TableRow({ className: "project-row", tabIndex: 0, "data-open-project": item.id, "aria-label": `Open project ${item.name}`, children: `${name}${runtime}${version}${updated}${actions}` });
    }).join(""),
  });
  return Table({ children: `${header}${body}` });
}

function renderDashboard() {
  const query = state.query.trim().toLowerCase();
  const visible = state.projects.filter((item) => !query || item.name.toLowerCase().includes(query) || item.type.toLowerCase().includes(query));
  const recent = visible.slice(0, 6);
  const list = state.dashboardSection === "dashboard" ? recent : visible;
  const projectSearch = InputGroup({
    className: "dashboard-search",
    children: `${Input({ id: "project-search", "aria-label": "Search projects", placeholder: "Search projects…", value: state.query })}${InputGroupAddon({ align: "inline-start", children: icon("search") })}`,
  });
  const navigation = [
    ["dashboard", "grid", "Dashboard"],
    ["projects", "stack", "Projects"],
  ].map(([section, sectionIcon, label]) => Button({
    className: `nav-item ${state.dashboardSection === section ? "active" : ""}`,
    variant: "ghost",
    "data-section": section,
    "aria-label": label,
    "aria-current": state.dashboardSection === section ? "page" : undefined,
    children: `${icon(sectionIcon)}<span>${label}</span>`,
  })).join("");
  const actions = state.dashboardSection === "dashboard" ? `<section class="dashboard-actions" aria-label="Project actions">
    ${Button({ className: "action-card", variant: "ghost", id: "new-project", children: `<span class="action-card-icon">${icon("plus", 24)}</span>${CardContent({ children: `${CardTitle({ children: "New project" })}${CardDescription({ children: "Choose an EdgeR starter and begin in the workbench." })}` })}${icon("chevron")}` })}
    ${Button({ className: "action-card", variant: "ghost", id: "import-project", children: `<span class="action-card-icon">${icon("import", 24)}</span>${CardContent({ children: `${CardTitle({ children: "Import" })}${CardDescription({ children: "Open a local project folder containing manifest.yaml." })}` })}${icon("chevron")}` })}
    ${Input({ hidden: true, id: "import-project-files", type: "file", multiple: true, webkitdirectory: true, directory: true })}
  </section>` : "";
  const projects = list.length
    ? renderProjectTable(list)
    : Empty({ className: "dashboard-empty", children: `${icon("stack", 28)}${EmptyTitle({ children: "No projects yet" })}${EmptyDescription({ children: "Create or import a project to open the workbench." })}` });
  document.querySelector("#app").innerHTML = `
    <main class="dashboard-shell">
      <header class="dashboard-topbar">
        <div class="dashboard-brand">${icon("logo", 24)}<strong>WebIDE</strong></div>
        ${projectSearch}
      </header>
      <aside class="dashboard-sidebar">
        <nav>${navigation}</nav>
      </aside>
      <section class="dashboard-content">
        <div class="dashboard-heading"><div><h1>${state.dashboardSection === "dashboard" ? "Build at the edge" : "Projects"}</h1><p>${state.dashboardSection === "dashboard" ? "Create, edit, deploy, and inspect EdgeR workers from one workspace." : "Local drafts stay in this browser until you deploy explicitly."}</p></div></div>
        ${actions}
        <section class="projects-section"><div class="section-title"><h2>${state.dashboardSection === "dashboard" ? "Recent projects" : "All projects"}</h2></div>${projects}</section>
      </section>
      ${renderTemplateModal()}
      ${renderProjectDialog()}
      ${renderToaster()}
    </main>`;
  bindDashboard();
}

function bindDashboard() {
  document.onkeydown = (event) => {
    if (event.key === "Escape" && state.projectDialog) { event.preventDefault(); closeProjectDialog(); }
    else if (event.key === "Escape" && state.templateModalOpen) { event.preventDefault(); closeTemplateModal(); }
    else if (state.projectDialog) trapDialogFocus(event, "#project-dialog");
    else if (state.templateModalOpen) trapDialogFocus(event, "#template-dialog");
  };
  document.querySelectorAll("[data-section]").forEach((button) => button.onclick = () => showDashboard(button.dataset.section));
  document.querySelector("#project-search").oninput = (event) => { state.query = event.target.value; renderDashboard(); document.querySelector("#project-search")?.focus(); };
  document.querySelector("#new-project").onclick = () => openTemplateModal("frontend");
  const importInput = document.querySelector("#import-project-files");
  document.querySelector("#import-project")?.addEventListener("click", () => importInput?.click());
  importInput?.addEventListener("change", () => importProject(importInput.files).catch((error) => showToast("Import failed", error.message, "error")));
  document.querySelectorAll("[data-template-category]").forEach((button) => button.onclick = () => openTemplateModal(button.dataset.templateCategory));
  document.querySelectorAll("[data-create-template]").forEach((button) => button.onclick = () => createNewProject(button.dataset.createTemplate));
  document.querySelector("#close-template-modal")?.addEventListener("click", closeTemplateModal);
  document.querySelector("[data-close-template-modal]")?.addEventListener("click", (event) => { if (event.target === event.currentTarget) closeTemplateModal(); });
  document.querySelectorAll("[data-open-project]").forEach((row) => {
    row.onclick = (event) => { if (!event.target.closest("[data-duplicate],[data-rename],[data-delete]")) openProject(row.dataset.openProject); };
    row.onkeydown = (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); openProject(row.dataset.openProject); } };
  });
  document.querySelectorAll("[data-duplicate]").forEach((button) => button.onclick = () => duplicateProject(button.dataset.duplicate));
  document.querySelectorAll("[data-rename]").forEach((button) => button.onclick = () => openProjectDialog("rename", button.dataset.rename));
  document.querySelectorAll("[data-delete]").forEach((button) => button.onclick = () => openProjectDialog("delete", button.dataset.delete));
  document.querySelector("#cancel-project-dialog")?.addEventListener("click", closeProjectDialog);
  document.querySelector("#project-dialog-overlay")?.addEventListener("pointerdown", (event) => { if (event.target === event.currentTarget) closeProjectDialog(); });
  document.querySelector("#project-dialog")?.addEventListener("submit", (event) => {
    event.preventDefault();
    if (document.querySelector("#project-dialog-input")) state.projectDialog.value = document.querySelector("#project-dialog-input").value;
    applyProjectDialog().catch((error) => { state.projectDialog.error = error.message; renderDashboard(); });
  });
}

function fileTree(files, explicitFolders = []) {
  const root = { children: [], path: "" };
  const folders = new Set(explicitFolders);
  for (const file of Object.keys(files)) {
    const parts = file.split("/");
    for (let index = 1; index < parts.length; index += 1) folders.add(parts.slice(0, index).join("/"));
  }
  const folderNodes = new Map([["", root]]);
  for (const path of [...folders].sort()) {
    const parts = path.split("/");
    const parentPath = parts.slice(0, -1).join("/");
    const parent = folderNodes.get(parentPath) || root;
    const node = { children: [], folder: true, name: parts.at(-1), path };
    folderNodes.set(path, node);
    parent.children.push(node);
  }
  for (const path of Object.keys(files)) {
    const parts = path.split("/");
    const parent = folderNodes.get(parts.slice(0, -1).join("/")) || root;
    parent.children.push({ folder: false, name: parts.at(-1), path });
  }
  const sortNodes = (nodes) => {
    nodes.sort((a, b) => Number(b.folder) - Number(a.folder) || a.name.localeCompare(b.name));
    nodes.filter((node) => node.folder).forEach((node) => sortNodes(node.children));
  };
  sortNodes(root.children);
  return root.children.map(renderTreeNode).join("");
}

function renderTreeNode(node) {
  if (node.folder) {
    const collapsed = state.collapsedFolders.has(node.path);
    const folder = Button({ variant: "ghost", "aria-expanded": !collapsed, "aria-label": `Folder ${node.path}`, className: "tree-row folder-row", "data-folder": node.path, role: "treeitem", children: `${icon("folder", 16)}<span class="tree-label">${escapeHtml(node.name)}</span>` });
    const menu = Button({ variant: "ghost", size: "icon-sm", className: "tree-row-menu", "data-tree-menu": node.path, "data-tree-kind": "folder", "aria-label": `Actions for ${node.name}`, children: icon("more", 13) });
    return `<div class="file-tree-group" role="group"><div class="tree-row-wrap">${folder}${menu}</div>${collapsed ? "" : `<div class="tree-children">${node.children.map(renderTreeNode).join("")}</div>`}</div>`;
  }
  const file = Button({ variant: "ghost", "aria-selected": state.selected === node.path, className: `tree-row file-row ${state.selected === node.path ? "active" : ""}`, "data-file": node.path, role: "treeitem", children: `${fileTypeIcon(node.path, 16)}<span class="tree-label">${escapeHtml(node.name)}</span>` });
  const menu = Button({ variant: "ghost", size: "icon-sm", className: "tree-row-menu", "data-tree-menu": node.path, "data-tree-kind": "file", "aria-label": `Actions for ${node.name}`, children: icon("more", 13) });
  return `<div class="tree-row-wrap">${file}${menu}</div>`;
}

function searchPanel(files) {
  const outcome = searchProjectFiles(files, state.searchQuery, { caseSensitive: state.searchCaseSensitive, regex: state.searchRegex });
  const summary = outcome.error || (!state.searchQuery ? "Type to search across project files." : outcome.matchCount ? `${outcome.matchCount} result${outcome.matchCount === 1 ? "" : "s"} in ${outcome.results.length} file${outcome.results.length === 1 ? "" : "s"}` : "No results.");
  const controls = InputGroup({ className: "search-field", children: `${Input({ id: "workspace-search", "aria-label": "Search files", placeholder: "Search", value: state.searchQuery })}${InputGroupAddon({ align: "inline-end", children: `${Button({ className: `search-toggle ${state.searchCaseSensitive ? "active" : ""}`, variant: "ghost", size: "icon-sm", id: "search-case", "aria-label": "Match case", "aria-pressed": state.searchCaseSensitive, title: "Match case", children: "Aa" })}${Button({ className: `search-toggle ${state.searchRegex ? "active" : ""}`, variant: "ghost", size: "icon-sm", id: "search-regex", "aria-label": "Use regular expression", "aria-pressed": state.searchRegex, title: "Use regular expression", children: ".*" })}` })}` });
  return `<div class="search-panel">
    ${controls}
    <p class="search-summary">${escapeHtml(summary)}</p>
    <div class="search-results">${outcome.results.map((result) => `<section class="search-group"><header>${fileTypeIcon(result.path)}<strong>${escapeHtml(result.path.split("/").at(-1))}</strong><small>${escapeHtml(result.path)}</small>${Badge({ children: result.matches.length })}</header>${result.matches.map((match) => Button({ className: "search-match", variant: "ghost", "data-search-file": result.path, "data-search-line": match.line, children: `<i>${match.line}</i><span>${escapeHtml(match.text.slice(0, match.start).trimStart())}<mark>${escapeHtml(match.text.slice(match.start, match.end))}</mark>${escapeHtml(match.text.slice(match.end))}</span>` })).join("")}</section>`).join("")}</div>
  </div>`;
}

function footerTabMeta(id) {
  return {
    problems: ["Problems", "check"],
    logs: ["Logs", "logs"],
    terminal: ["Terminal", "terminal"],
    deployments: ["Deployments", "deploy"],
  }[id];
}

function openFileDialog(kind, options = {}) {
  state.fileDialog = { kind, value: "", returnFocus: focusSelector(document.activeElement), ...options };
  renderWorkbench();
  requestAnimationFrame(() => document.querySelector("#file-dialog-input")?.focus());
}

function closeFileDialog() {
  const returnFocus = state.fileDialog?.returnFocus;
  state.fileDialog = null;
  renderWorkbench();
  if (returnFocus) requestAnimationFrame(() => document.querySelector(returnFocus)?.focus());
}

function renderFileDialog() {
  const dialog = state.fileDialog;
  if (!dialog) return "";
  const titles = {
    "create-file": dialog.basePath ? `New file in ${dialog.basePath}` : "New file",
    "create-folder": dialog.basePath ? `New folder in ${dialog.basePath}` : "New folder",
    "rename-file": "Rename file",
    "rename-folder": "Rename folder",
    "delete-file": "Delete file",
    "delete-folder": "Delete folder",
  };
  const deleting = dialog.kind.startsWith("delete-");
  const parts = deleting
    ? { Root: AlertDialog, Content: AlertDialogContent, Header: AlertDialogHeader, Title: AlertDialogTitle, Description: AlertDialogDescription, Footer: AlertDialogFooter }
    : { Root: Dialog, Content: DialogContent, Header: DialogHeader, Title: DialogTitle, Description: DialogDescription, Footer: DialogFooter };
  const header = parts.Header({ children: `${parts.Title({ id: "file-dialog-title", children: escapeHtml(titles[dialog.kind]) })}${deleting ? parts.Description({ children: `Delete <strong>${escapeHtml(dialog.path)}</strong>? This cannot be undone.` }) : ""}` });
  const field = deleting ? "" : Field({ children: FieldLabel({ htmlFor: "file-dialog-input", children: `Name${Input({ id: "file-dialog-input", autocomplete: "off", value: dialog.value, "aria-invalid": Boolean(dialog.error) })}` }) });
  const error = dialog.error ? FieldError({ className: "dialog-error", children: escapeHtml(dialog.error) }) : "";
  const footer = parts.Footer({ children: `${Button({ className: "button", variant: "outline", id: "cancel-file-dialog", children: "Cancel" })}${Button({ className: `button ${deleting ? "danger" : "primary"}`, variant: deleting ? "destructive" : "default", type: "submit", children: deleting ? "Delete" : "Confirm" })}` });
  return parts.Root({ className: "workbench-dialog-overlay", id: "file-dialog-overlay", children: parts.Content({ className: "workbench-dialog", id: "file-dialog", "aria-labelledby": "file-dialog-title", children: `${header}${field}${error}${footer}` }) });
}

function joinProjectPath(basePath, name) {
  return [basePath, name.trim()].filter(Boolean).join("/");
}

function applyFileDialog() {
  const active = project();
  const dialog = state.fileDialog;
  if (!active || !dialog) return;
  active.folders ||= [];
  if (dialog.kind === "create-file" || dialog.kind === "create-folder") {
    const path = joinProjectPath(dialog.basePath, dialog.value);
    validateFilePath(path);
    if (active.files[path] !== undefined || active.folders.includes(path)) throw new Error(`Path already exists: ${path}`);
    if (dialog.kind === "create-file") {
      active.files[path] = "";
      state.selected = path;
      if (!state.openTabs.includes(path)) state.openTabs.push(path);
    } else {
      active.folders.push(path);
    }
  } else if (dialog.kind === "rename-file") {
    const parent = dialog.path.includes("/") ? dialog.path.slice(0, dialog.path.lastIndexOf("/")) : "";
    const nextPath = joinProjectPath(parent, dialog.value);
    validateFilePath(nextPath);
    if (nextPath !== dialog.path && (active.files[nextPath] !== undefined || active.folders.includes(nextPath))) throw new Error(`Path already exists: ${nextPath}`);
    active.files[nextPath] = active.files[dialog.path];
    delete active.files[dialog.path];
    state.openTabs = state.openTabs.map((path) => path === dialog.path ? nextPath : path);
    if (state.selected === dialog.path) state.selected = nextPath;
  } else if (dialog.kind === "rename-folder") {
    const parent = dialog.path.includes("/") ? dialog.path.slice(0, dialog.path.lastIndexOf("/")) : "";
    const nextPath = joinProjectPath(parent, dialog.value);
    validateFilePath(nextPath);
    if (nextPath !== dialog.path && (active.files[nextPath] !== undefined || active.folders.includes(nextPath))) throw new Error(`Path already exists: ${nextPath}`);
    const move = (path) => path === dialog.path || path.startsWith(`${dialog.path}/`) ? `${nextPath}${path.slice(dialog.path.length)}` : path;
    active.files = Object.fromEntries(Object.entries(active.files).map(([path, value]) => [move(path), value]));
    active.folders = active.folders.map(move);
    state.openTabs = state.openTabs.map(move);
    state.selected = move(state.selected);
  } else if (dialog.kind === "delete-file") {
    delete active.files[dialog.path];
    state.openTabs = state.openTabs.filter((path) => path !== dialog.path);
  } else if (dialog.kind === "delete-folder") {
    active.files = Object.fromEntries(Object.entries(active.files).filter(([path]) => !path.startsWith(`${dialog.path}/`)));
    active.folders = active.folders.filter((path) => path !== dialog.path && !path.startsWith(`${dialog.path}/`));
    state.openTabs = state.openTabs.filter((path) => active.files[path] !== undefined);
  }
  if (active.files[state.selected] === undefined) state.selected = state.openTabs.at(-1) || Object.keys(active.files)[0] || "";
  active.selected = state.selected;
  active.openTabs = [...state.openTabs];
  state.fileDialog = null;
  persistWorkbenchState(active);
  scheduleSave();
  renderWorkbench();
}

function renderWorkbench() {
  document.onkeydown = null;
  const active = project();
  if (!active) return showDashboard();
  const selected = active.files[state.selected] !== undefined ? state.selected : state.openTabs.find((file) => active.files[file] !== undefined) || "";
  state.selected = selected;
  if (selected && !state.openTabs.includes(selected)) state.openTabs.push(selected);
  const content = selected ? active.files[selected] || "" : "";
  const lines = content.split("\n").length;
  const diagnostics = selected ? lintDocument(selected, content, active.files) : [];
  const iconAction = (id, actionIcon, label, options = {}) => Button({
    className: `icon-button ${options.active ? "active" : ""}`,
    variant: "ghost",
    size: "icon-sm",
    id,
    title: label,
    "aria-label": label,
    "aria-pressed": options.pressed,
    disabled: options.disabled,
    children: icon(actionIcon, options.iconSize),
  });
  const workbenchActions = [
    iconAction("toggle-preview", "eye", `${state.previewVisible ? "Hide" : "Show"} preview`, { active: state.previewVisible, pressed: state.previewVisible }),
    iconAction("toggle-footer", "terminal", `${state.footerVisible ? "Hide" : "Show"} panel`, { active: state.footerVisible, pressed: state.footerVisible }),
    iconAction("validate-project", "check", "Validate project"),
    iconAction("deploy", "deploy", state.deploying ? "Deploying" : "Deploy project", { disabled: state.deploying }),
  ].join("");
  const activity = [
    ["files", "file", "Explorer"],
    ["search", "search", "Search (Cmd/Ctrl+Shift+F)"],
  ].map(([view, viewIcon, label]) => Button({
    className: `activity ${state.sidebarView === view ? "active" : ""}`,
    variant: "ghost",
    size: "icon",
    id: `activity-${view}`,
    title: label,
    "aria-label": label,
    "aria-pressed": state.sidebarView === view,
    children: icon(viewIcon, 19),
  })).join("");
  const paneActions = state.sidebarView === "files" ? `<span>${iconAction("add-file", "filePlus", "New file", { iconSize: 16 })}${iconAction("add-folder", "folderPlus", "New folder", { iconSize: 16 })}</span>` : "";
  const editorTabs = TabsList({
    className: "editor-tabs",
    children: state.openTabs.filter((file) => active.files[file] !== undefined).map((file) => Tooltip({
      content: escapeHtml(file),
      children: `<div class="editor-tab ${file === selected ? "active" : ""}" data-editor-tab="${escapeHtml(file)}">${TabsTrigger({ active: file === selected, title: file, tabIndex: 0, children: `${fileTypeIcon(file, 16)}<span>${escapeHtml(file.split("/").at(-1))}</span>` })}${Button({ className: "editor-tab-close", variant: "ghost", size: "icon-sm", "data-close-tab": file, "aria-label": `Close ${file.split("/").at(-1)}`, children: icon("close", 12) })}</div>`,
    })).join(""),
  });
  const editorSurface = selected
    ? `<div class="code-editor"><div class="line-numbers" id="line-numbers">${Array.from({ length: lines }, (_, index) => `<span>${index + 1}</span>`).join("")}</div><div class="editor-stack"><pre class="syntax-layer" id="syntax-layer" aria-hidden="true">${highlightCode(selected, content)}</pre>${Textarea({ id: "editor", "aria-label": `${selected} editor`, spellcheck: false })}</div></div>`
    : Empty({ className: "editor-empty", children: EmptyDescription({ children: "Open a file from the Explorer." }) });
  const previewActions = `${iconAction("refresh-preview", "refresh", "Refresh preview")}${active.previewUrl
    ? ButtonLink({ className: "icon-button", variant: "ghost", size: "icon-sm", id: "open-preview", href: active.previewUrl, target: "_blank", rel: "noopener noreferrer", title: "Open in new tab", "aria-label": "Open in new tab", children: icon("external") })
    : iconAction("open-preview", "external", "Open in new tab", { disabled: true })}`;
  const previewContent = active.previewUrl
    ? `<iframe id="preview-frame" sandbox="allow-forms allow-modals allow-popups allow-scripts" src="${escapeHtml(active.previewUrl)}"></iframe>`
    : Empty({ className: "preview-empty", children: `${icon("eye", 24)}${EmptyTitle({ children: "No deployment to preview" })}${EmptyDescription({ children: "Autosave stores only the draft. Deploy explicitly to update this panel." })}${Button({ className: "button primary", id: "preview-deploy", children: "Deploy project" })}` });
  const footerTabs = TabsList({
    children: state.footerOrder.map((tab) => {
      const [label, tabIcon] = footerTabMeta(tab);
      return TabsTrigger({ active: state.footerTab === tab, className: `console-tab ${state.footerTab === tab ? "active" : ""}`, "data-footer-tab": tab, children: `${icon(tabIcon, 14)}${label}` });
    }).join(""),
  });
  const preserveLogs = state.footerTab === "logs" ? FieldLabel({ className: "preserve-logs", htmlFor: "preserve-logs", children: `${Checkbox({ id: "preserve-logs", checked: state.preserveLogs })}<span>Preserve logs across restarts</span>` }) : "";
  document.querySelector("#app").innerHTML = `
    <main class="workbench-shell" style="--explorer-width:${localStorage.getItem("edger.webide.explorerWidth") || "190"}px">
      <header class="workbench-topbar">
        ${Button({ className: "workbench-brand", variant: "ghost", size: "icon", id: "workbench-home", "aria-label": "Open WebIDE dashboard", title: "Open WebIDE dashboard", children: icon("logo", 19) })}
        <div></div><div class="project-identity"><strong>${escapeHtml(active.name)}</strong></div>
        <div class="workbench-actions">${workbenchActions}</div>
      </header>
      <div class="workbench-body">
        <aside class="activity-bar">${activity}</aside>
        <aside class="explorer ${state.sidebarView === "search" ? "search-open" : ""}"><div class="pane-title"><span>${state.sidebarView === "search" ? "SEARCH" : "EXPLORER"}</span>${paneActions}</div>${state.sidebarView === "files" ? `<div class="file-tree" role="tree" aria-label="Workspace file tree">${fileTree(active.files, active.folders)}</div>` : searchPanel(active.files)}</aside>
        ${ResizableHandle({ className: "splitter vertical explorer-splitter", id: "explorer-splitter", orientation: "vertical", "aria-label": "Resize Explorer" })}
        <section class="workbench-main ${state.previewVisible ? "" : "preview-hidden"} ${state.footerVisible ? "" : "footer-hidden"}" style="--preview-width:${localStorage.getItem("edger.webide.previewWidth") || "40"}%;--footer-height:${localStorage.getItem("edger.webide.footerHeight") || "28"}%">
          <div class="editor-preview-row">
            <section class="editor-area">
              ${editorTabs}
              ${editorSurface}
            </section>
            ${ResizableHandle({ className: "splitter vertical", id: "preview-splitter", orientation: "vertical", "aria-label": "Resize Preview" })}
            <aside class="preview-area"><div class="preview-toolbar"><strong>Preview</strong><div>${previewActions}</div></div>${previewContent}</aside>
          </div>
          ${ResizableHandle({ className: "splitter horizontal", id: "footer-splitter", orientation: "horizontal", "aria-label": "Resize panel" })}
          <section class="console-panel"><header>${footerTabs}${preserveLogs}</header>${TabsContent({ id: "console-content", children: renderConsoleContent(active, diagnostics) })}</section>
        </section>
      </div>
    </main>${renderFileDialog()}${renderToaster()}`;
  bindWorkbench();
}

function renderConsoleContent(active, diagnostics = lintDocument(state.selected, active.files[state.selected] || "", active.files)) {
  if (state.footerTab === "problems") {
    return diagnostics.length ? `<div class="problems-output">${diagnostics.map((item) => Button({ className: "problem-row", variant: "ghost", "data-problem-line": item.line, children: `<span class="problem-severity ${escapeHtml(item.severity)}">!</span><p>${escapeHtml(item.message)}</p><small>${escapeHtml(state.selected)}:${item.line}</small>` })).join("")}</div>` : Empty({ className: "console-empty", children: EmptyDescription({ children: "No problems detected in the active file." }) });
  }
  if (state.footerTab === "logs") {
    const logs = active.logs || [];
    return `<div class="log-output">${logs.length ? logs.map((line) => `<div class="log-line ${escapeHtml(line.level)}"><time>${escapeHtml(new Date(line.at).toLocaleTimeString())}</time><span>${escapeHtml(line.source)}</span><p>${escapeHtml(line.message)}</p></div>`).join("") : Empty({ className: "console-empty", children: EmptyDescription({ children: "No local events yet." }) })}</div>`;
  }
  if (state.footerTab === "deployments") {
    return `<div class="deploy-console"><div id="deploy-progress">${deployProgressHtml()}</div>${(active.deployments || []).map((item) => `<div class="deployment-row"><time>${escapeHtml(new Date(item.at).toLocaleString())}</time><strong class="${item.status === "Succeeded" ? "success" : "error"}">${escapeHtml(item.status)}</strong><span>${escapeHtml(item.message || [item.release, item.health, item.activation].filter(Boolean).join(" · ") || "Deployment completed")}</span></div>`).join("") || Empty({ className: "console-empty", children: EmptyDescription({ children: "No deployments yet." }) })}</div>`;
  }
  const terminalInput = InputGroup({
    as: "form",
    className: "terminal-input",
    id: "terminal-form",
    children: `${Input({ id: "terminal-command", autocomplete: "off", "aria-label": "Operational command", placeholder: "help, validate, deploy, preview, files, status, clear" })}${InputGroupAddon({ align: "inline-start", children: "$" })}${InputGroupAddon({ align: "inline-end", children: Button({ className: "terminal-run", variant: "ghost", size: "icon-sm", type: "submit", "aria-label": "Run operational command", children: icon("chevron") }) })}`,
  });
  return `<div class="terminal-output"><div class="terminal-banner">EdgeR operational console · type <code>help</code> for safe workspace commands. This is not a host shell.</div>${state.terminalHistory.map((entry) => `<div class="terminal-entry ${escapeHtml(entry.kind || "")}"><span>${entry.prompt ? "$" : "›"}</span><pre>${escapeHtml(entry.text)}</pre></div>`).join("")}</div>${terminalInput}`;
}

function deployProgressHtml() {
  if (!state.deploySteps.length) return "";
  return `<div class="deploy-steps">${state.deploySteps.map((step) => `<div class="step ${escapeHtml(step.status)}"><span>${step.status === "done" ? "✓" : step.status === "failed" ? "×" : step.status === "active" ? "●" : "○"}</span>${escapeHtml(step.label)}</div>`).join("")}</div>`;
}

function renderDeployProgress() {
  const target = document.querySelector("#deploy-progress");
  if (target) target.innerHTML = deployProgressHtml();
}

function renderStatus() {
  const target = document.querySelector("#draft-status");
  if (target) target.textContent = state.saving ? "Saving draft…" : state.message;
}

function selectFile(file) {
  const active = project();
  if (!active || active.files[file] === undefined) return;
  active.selected = file;
  state.selected = file;
  if (!state.openTabs.includes(file)) state.openTabs.push(file);
  active.openTabs = [...state.openTabs];
  persistWorkbenchState(active);
  renderWorkbench();
}

function closeTab(file) {
  const active = project();
  const index = state.openTabs.indexOf(file);
  state.openTabs = state.openTabs.filter((item) => item !== file);
  if (state.selected === file) state.selected = state.openTabs[index] || state.openTabs[index - 1] || "";
  if (active) { active.selected = state.selected; active.openTabs = [...state.openTabs]; persistWorkbenchState(active); }
  renderWorkbench();
}

function reorderEditorTabs(from, target, side) {
  if (!from) return;
  state.openTabs = reorderItems(state.openTabs, from, target, side);
  const active = project();
  if (active) { active.openTabs = [...state.openTabs]; persistWorkbenchState(active); }
}

function reorderFooterTabs(from, target) {
  if (!from || from === target) return;
  const fromIndex = state.footerOrder.indexOf(from);
  const targetIndex = state.footerOrder.indexOf(target);
  if (fromIndex < 0 || targetIndex < 0) return;
  const next = reorderItems(state.footerOrder, from, target, fromIndex < targetIndex ? "after" : "before");
  state.footerOrder = next;
  localStorage.setItem("edger.webide.footerOrder", JSON.stringify(next));
}

function bindPointerReorder(elements, options) {
  const clearIndicators = () => document.querySelectorAll(options.indicatorSelector).forEach((item) => {
    options.indicatorClasses.forEach((className) => item.classList.remove(className));
  });
  elements.forEach((element) => {
    element.onpointerdown = (event) => {
      const nestedControl = event.target.closest("button,input");
      if (event.button !== 0 || nestedControl && nestedControl !== element) return;
      const source = options.key(element);
      const start = { x: event.clientX, y: event.clientY };
      let dragging = false;
      let target = null;
      let side = "after";
      const move = (pointerEvent) => {
        if (!dragging && Math.hypot(pointerEvent.clientX - start.x, pointerEvent.clientY - start.y) < 5) return;
        dragging = true;
        pointerEvent.preventDefault();
        clearIndicators();
        target = document.elementFromPoint(pointerEvent.clientX, pointerEvent.clientY)?.closest(options.targetSelector) || null;
        if (!target || options.key(target) === source) return;
        const rect = target.getBoundingClientRect();
        side = pointerEvent.clientX < rect.left + rect.width / 2 ? "before" : "after";
        target.classList.add(options.indicator(side));
      };
      const end = (pointerEvent) => {
        document.removeEventListener("pointermove", move);
        document.removeEventListener("pointerup", end);
        document.removeEventListener("pointercancel", end);
        clearIndicators();
        if (!dragging) return;
        pointerEvent.preventDefault();
        suppressReorderClick = true;
        if (target && options.key(target) !== source) options.drop(source, options.key(target), side);
        renderWorkbench();
        setTimeout(() => { suppressReorderClick = false; }, 0);
      };
      document.addEventListener("pointermove", move, { passive: false });
      document.addEventListener("pointerup", end);
      document.addEventListener("pointercancel", end);
    };
  });
}

function openTreeMenu(event, path, kind) {
  event.preventDefault();
  document.querySelector("#tree-context-menu")?.remove();
  const basename = path.split("/").at(-1);
  const item = (label, action, destructive = false) => ContextMenuItem({ children: label, "data-menu-action": action, destructive, className: destructive ? "destructive" : "" });
  const children = kind === "folder"
    ? `${item("New file…", "new-file")}${item("New folder…", "new-folder")}${ContextMenuSeparator()}${item("Rename…", "rename")}${item("Delete", "delete", true)}`
    : `${item("Rename…", "rename")}${item("Delete", "delete", true)}`;
  const host = document.createElement("div");
  host.innerHTML = ContextMenuContent({
    id: "tree-context-menu",
    className: "tree-context-menu",
    style: `left:${Math.min(event.clientX, innerWidth - 190)}px;top:${Math.min(event.clientY, innerHeight - 190)}px`,
    children,
  });
  const menu = host.firstElementChild;
  document.body.append(menu);
  const actions = {
    "new-file": () => openFileDialog("create-file", { basePath: path, returnFocus: `[data-tree-menu="${CSS.escape(path)}"]` }),
    "new-folder": () => openFileDialog("create-folder", { basePath: path, returnFocus: `[data-tree-menu="${CSS.escape(path)}"]` }),
    rename: () => {
      if (kind === "file") {
        const active = project();
        state.selected = path;
        if (active) {
          active.selected = path;
          if (!state.openTabs.includes(path)) state.openTabs.push(path);
          active.openTabs = [...state.openTabs];
        }
      }
      openFileDialog(`rename-${kind}`, { path, value: basename, returnFocus: `[data-tree-menu="${CSS.escape(path)}"]` });
    },
    delete: () => openFileDialog(`delete-${kind}`, { path, returnFocus: `[data-tree-menu="${CSS.escape(path)}"]` }),
  };
  menu.querySelectorAll("[data-menu-action]").forEach((button) => button.onclick = () => { menu.remove(); actions[button.dataset.menuAction](); });
  const dismiss = (pointerEvent) => {
    if (menu.contains(pointerEvent.target)) return;
    menu.remove();
    document.removeEventListener("pointerdown", dismiss);
  };
  setTimeout(() => document.addEventListener("pointerdown", dismiss), 0);
}

function bindWorkbench() {
  const active = project();
  document.querySelector("#workbench-home").onclick = () => showDashboard("dashboard");
  document.querySelector("#deploy").onclick = deploy;
  document.querySelector("#toggle-preview").onclick = () => { state.previewVisible = !state.previewVisible; localStorage.setItem("edger.webide.previewVisible", state.previewVisible); renderWorkbench(); };
  document.querySelector("#toggle-footer").onclick = () => { state.footerVisible = !state.footerVisible; localStorage.setItem("edger.webide.footerVisible", state.footerVisible); renderWorkbench(); };
  document.querySelector("#preview-deploy")?.addEventListener("click", deploy);
  document.querySelector("#validate-project").onclick = () => {
    try { validateProject(active); appendLog("Project validation passed.", "success", "VALIDATE"); state.message = "Validation passed"; state.toast = { title: "Validation passed", description: "The project manifest and entrypoint are valid.", variant: "success" }; }
    catch (error) { appendLog(error.message, "error", "VALIDATE"); state.message = error.message; state.toast = { title: "Validation failed", description: error.message, variant: "error" }; }
    state.footerTab = "logs"; state.footerVisible = true; localStorage.setItem("edger.webide.footerVisible", "true"); renderWorkbench();
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { state.toast = null; syncToaster(); }, 4500);
  };
  document.querySelector("#refresh-preview")?.addEventListener("click", () => { const frame = document.querySelector("#preview-frame"); if (frame) frame.src = frame.src; });
  document.querySelector("#activity-files").onclick = () => { state.sidebarView = "files"; renderWorkbench(); };
  document.querySelector("#activity-search").onclick = () => { state.sidebarView = "search"; renderWorkbench(); requestAnimationFrame(() => document.querySelector("#workspace-search")?.focus()); };
  const workspaceSearch = document.querySelector("#workspace-search");
  if (workspaceSearch) workspaceSearch.oninput = () => { state.searchQuery = workspaceSearch.value; renderWorkbench(); requestAnimationFrame(() => { const input = document.querySelector("#workspace-search"); input?.focus(); input?.setSelectionRange(state.searchQuery.length, state.searchQuery.length); }); };
  document.querySelector("#search-case")?.addEventListener("click", () => { state.searchCaseSensitive = !state.searchCaseSensitive; renderWorkbench(); });
  document.querySelector("#search-regex")?.addEventListener("click", () => { state.searchRegex = !state.searchRegex; renderWorkbench(); });
  document.querySelectorAll("[data-search-file]").forEach((button) => button.onclick = () => selectFileAtLine(button.dataset.searchFile, Number(button.dataset.searchLine)));
  document.querySelectorAll("[data-file]").forEach((button) => button.onclick = () => selectFile(button.dataset.file));
  document.querySelectorAll("[data-folder]").forEach((button) => button.onclick = () => { const path = button.dataset.folder; state.collapsedFolders.has(path) ? state.collapsedFolders.delete(path) : state.collapsedFolders.add(path); renderWorkbench(); });
  document.querySelectorAll(".tree-row-wrap").forEach((row) => row.oncontextmenu = (event) => { const trigger = row.querySelector("[data-tree-menu]"); openTreeMenu(event, trigger.dataset.treeMenu, trigger.dataset.treeKind); });
  document.querySelectorAll("[data-tree-menu]").forEach((button) => button.onclick = (event) => { event.stopPropagation(); const rect = button.getBoundingClientRect(); openTreeMenu({ preventDefault() {}, clientX: rect.right, clientY: rect.bottom }, button.dataset.treeMenu, button.dataset.treeKind); });
  const editorTabs = document.querySelectorAll("[data-editor-tab]");
  editorTabs.forEach((tab) => {
    tab.onclick = (event) => { if (!suppressReorderClick && !event.target.closest("[data-close-tab]")) selectFile(tab.dataset.editorTab); };
    tab.onauxclick = (event) => { if (event.button === 1) closeTab(tab.dataset.editorTab); };
    tab.onkeydown = (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); selectFile(tab.dataset.editorTab); } };
  });
  bindPointerReorder(editorTabs, {
    key: (tab) => tab.dataset.editorTab,
    targetSelector: "[data-editor-tab]",
    indicatorSelector: ".drop-before,.drop-after",
    indicatorClasses: ["drop-before", "drop-after"],
    indicator: (side) => `drop-${side}`,
    drop: reorderEditorTabs,
  });
  document.querySelectorAll("[data-close-tab]").forEach((button) => button.onclick = (event) => { event.stopPropagation(); closeTab(button.dataset.closeTab); });
  document.querySelector("#add-file")?.addEventListener("click", () => openFileDialog("create-file", { basePath: "" }));
  document.querySelector("#add-folder")?.addEventListener("click", () => openFileDialog("create-folder", { basePath: "" }));
  const editor = document.querySelector("#editor");
  if (editor) {
    editor.value = active.files[state.selected] || "";
    editor.oninput = () => { active.files[state.selected] = editor.value; updateEditorSurface(editor.value); scheduleSave(); };
    editor.onkeydown = (event) => {
      if (event.key === "Tab") { event.preventDefault(); const start = editor.selectionStart; editor.setRangeText("  ", start, editor.selectionEnd, "end"); editor.dispatchEvent(new Event("input")); }
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") { event.preventDefault(); clearTimeout(saveTimer); saveProject(active).then(() => { state.dirty = false; state.message = "Draft saved locally"; renderStatus(); }); }
    };
    editor.onscroll = syncEditorScroll;
  }
  const footerTabs = document.querySelectorAll("[data-footer-tab]");
  footerTabs.forEach((button) => {
    button.onclick = () => { if (suppressReorderClick) return; state.footerTab = button.dataset.footerTab; state.footerVisible = true; localStorage.setItem("edger.webide.footerVisible", "true"); renderWorkbench(); };
  });
  bindPointerReorder(footerTabs, {
    key: (button) => button.dataset.footerTab,
    targetSelector: "[data-footer-tab]",
    indicatorSelector: ".panel-tab-drop-target",
    indicatorClasses: ["panel-tab-drop-target"],
    indicator: () => "panel-tab-drop-target",
    drop: reorderFooterTabs,
  });
  document.querySelector("#preserve-logs")?.addEventListener("change", (event) => { state.preserveLogs = event.target.checked; localStorage.setItem("edger.webide.preserveLogs", state.preserveLogs); });
  document.querySelectorAll("[data-problem-line]").forEach((button) => button.onclick = () => focusEditorLine(Number(button.dataset.problemLine)));
  document.querySelector("#cancel-file-dialog")?.addEventListener("click", closeFileDialog);
  document.querySelector("#file-dialog-overlay")?.addEventListener("pointerdown", (event) => { if (event.target === event.currentTarget) closeFileDialog(); });
  document.querySelector("#file-dialog")?.addEventListener("submit", (event) => { event.preventDefault(); if (document.querySelector("#file-dialog-input")) state.fileDialog.value = document.querySelector("#file-dialog-input").value; try { applyFileDialog(); } catch (error) { state.fileDialog.error = error.message; renderWorkbench(); } });
  const terminal = document.querySelector("#terminal-form");
  if (terminal) terminal.onsubmit = (event) => { event.preventDefault(); runCommand(document.querySelector("#terminal-command").value); };
  bindSplitters();
  document.onkeydown = (event) => {
    if (event.key === "Escape" && state.fileDialog) { event.preventDefault(); closeFileDialog(); }
    else if (state.fileDialog) trapDialogFocus(event, "#file-dialog");
    else if ((event.metaKey || event.ctrlKey) && event.shiftKey && event.key.toLowerCase() === "f") {
      event.preventDefault(); state.sidebarView = "search"; renderWorkbench(); requestAnimationFrame(() => document.querySelector("#workspace-search")?.focus());
    }
  };
}

function updateEditorSurface(value) {
  const target = document.querySelector("#line-numbers");
  if (target) target.innerHTML = Array.from({ length: value.split("\n").length }, (_, index) => `<span>${index + 1}</span>`).join("");
  const syntax = document.querySelector("#syntax-layer");
  if (syntax) syntax.innerHTML = highlightCode(state.selected, value);
  const active = project();
  const diagnostics = active ? lintDocument(state.selected, value, active.files) : [];
  const consoleContent = document.querySelector("#console-content");
  if (active && consoleContent && state.footerTab === "problems") {
    consoleContent.innerHTML = renderConsoleContent(active, diagnostics);
    consoleContent.querySelectorAll("[data-problem-line]").forEach((button) => {
      button.onclick = () => focusEditorLine(Number(button.dataset.problemLine));
    });
  }
  const problemStatus = document.querySelector("#open-problems");
  if (problemStatus) {
    problemStatus.textContent = diagnostics.length ? `${diagnostics.length} problem${diagnostics.length === 1 ? "" : "s"}` : "No problems";
    problemStatus.classList.toggle("has-problems", diagnostics.length > 0);
  }
  syncEditorScroll();
}

function syncEditorScroll() {
  const editor = document.querySelector("#editor");
  if (!editor) return;
  const lines = document.querySelector("#line-numbers");
  const syntax = document.querySelector("#syntax-layer");
  if (lines) lines.scrollTop = editor.scrollTop;
  if (syntax) { syntax.scrollTop = editor.scrollTop; syntax.scrollLeft = editor.scrollLeft; }
}

function selectFileAtLine(file, line) {
  selectFile(file);
  requestAnimationFrame(() => focusEditorLine(line));
}

function focusEditorLine(line) {
  const editor = document.querySelector("#editor");
  if (!editor) return;
  const content = editor.value;
  let offset = 0;
  for (let current = 1; current < line; current += 1) offset = content.indexOf("\n", offset) + 1;
  editor.focus();
  editor.setSelectionRange(Math.max(0, offset), Math.max(0, offset));
  const lineHeight = parseFloat(getComputedStyle(editor).lineHeight) || 19.84;
  editor.scrollTop = Math.max(0, (line - 3) * lineHeight);
  syncEditorScroll();
}

function bindSplitters() {
  const main = document.querySelector(".workbench-main");
  const shell = document.querySelector(".workbench-shell");
  const explorer = document.querySelector("#explorer-splitter");
  const vertical = document.querySelector("#preview-splitter");
  const horizontal = document.querySelector("#footer-splitter");
  const bind = (handle, move, end) => {
    if (!handle) return;
    handle.onpointerdown = (event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      const onMove = (pointerEvent) => { pointerEvent.preventDefault(); move(pointerEvent); };
      const onEnd = () => {
        document.removeEventListener("pointermove", onMove);
        document.removeEventListener("pointerup", onEnd);
        document.removeEventListener("pointercancel", onEnd);
        end();
      };
      document.addEventListener("pointermove", onMove, { passive: false });
      document.addEventListener("pointerup", onEnd);
      document.addEventListener("pointercancel", onEnd);
    };
  };
  bind(explorer,
    (move) => { const width = Math.min(420, Math.max(180, move.clientX - 48)); shell.style.setProperty("--explorer-width", `${width}px`); },
    () => localStorage.setItem("edger.webide.explorerWidth", parseFloat(getComputedStyle(shell).getPropertyValue("--explorer-width"))),
  );
  bind(vertical,
    (move) => { const rect = main.getBoundingClientRect(); const preview = Math.min(65, Math.max(24, ((rect.right - move.clientX) / rect.width) * 100)); main.style.setProperty("--preview-width", `${preview}%`); },
    () => localStorage.setItem("edger.webide.previewWidth", parseFloat(getComputedStyle(main).getPropertyValue("--preview-width"))),
  );
  bind(horizontal,
    (move) => { const rect = main.getBoundingClientRect(); const footer = Math.min(48, Math.max(16, ((rect.bottom - move.clientY) / rect.height) * 100)); main.style.setProperty("--footer-height", `${footer}%`); },
    () => localStorage.setItem("edger.webide.footerHeight", parseFloat(getComputedStyle(main).getPropertyValue("--footer-height"))),
  );
}

function runCommand(raw) {
  const command = raw.trim();
  if (!command) return;
  state.terminalHistory.push({ prompt: true, text: command });
  const [name] = command.split(/\s+/);
  const active = project();
  const output = (text, kind = "") => state.terminalHistory.push({ prompt: false, text, kind });
  if (name === "help") output("Available: help, validate, deploy, preview, files, status, clear\nCommands operate on this EdgeR project only; no host shell is exposed.");
  else if (name === "files") output(Object.keys(active.files).sort().join("\n"));
  else if (name === "status") output(`project=${active.name}\nversion=${active.version}\ndraft=${state.dirty ? "unsaved" : "saved"}\npreview=${active.previewUrl || "not deployed"}`);
  else if (name === "preview") output(active.previewUrl || "No successful deployment available.");
  else if (name === "validate") { try { const value = validateProject(active); output(`Valid: ${value.name}@${value.version}`, "success"); } catch (error) { output(error.message, "error"); } }
  else if (name === "deploy") { output("Starting explicit EdgeR deployment…"); state.footerTab = "deployments"; renderWorkbench(); void deploy(); return; }
  else if (name === "clear") state.terminalHistory = [];
  else output(`Unknown command: ${name}. Type help.`, "error");
  state.terminalHistory = state.terminalHistory.slice(-100);
  renderWorkbench();
  setTimeout(() => document.querySelector("#terminal-command")?.focus(), 0);
}

function showError(error) {
  state.message = error.message;
  renderStatus();
  showToast("Something went wrong", error.message, "error");
}

async function initialize() {
  try {
    state.projects = await loadProjects();
    const requested = new URLSearchParams(location.search).get("project");
    if (requested && state.projects.some((item) => item.id === requested)) openProject(requested);
    else render();
  } catch (error) {
    state.message = `Local storage unavailable: ${error.message}`;
    render();
  }
}

initialize();
