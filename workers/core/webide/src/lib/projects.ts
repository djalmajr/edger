export type ProjectType =
  | "FetchHandler"
  | "RoutesTable"
  | "StaticSpa"
  | "React"
  | "Vue"
  | "Svelte"
  | "TanStackStart"
  | "NextJs";

export type ProjectLog = {
  at: string;
  level: string;
  message: string;
  source: string;
};
export type Deployment = {
  at: string;
  message?: string;
  previewUrl?: string;
  status: string;
  [key: string]: unknown;
};
export type Project = {
  createdAt: string;
  deployments: Deployment[];
  files: Record<string, string>;
  folders: string[];
  id: string;
  logs: ProjectLog[];
  name: string;
  openTabs?: string[];
  previewUrl: string;
  selected: string;
  settings: Record<string, unknown>;
  type: ProjectType;
  updatedAt: string;
  version: string;
};

export type Template = {
  category: "backend" | "frontend" | "fullstack";
  description: string;
  files?: Record<string, string>;
  name: string;
  runtime: string;
  supported: boolean;
};

export const templates: Record<ProjectType, Template> = {
  FetchHandler: {
    category: "backend",
    description: "Request handler with a persistent Deno process.",
    name: "Fetch Handler",
    runtime: "EdgeR · Deno",
    supported: true,
    files: {
      "manifest.yaml":
        'name: hello-webide\nversion: "1.0.0"\nentrypoint: index.ts\nkind: fetch\n',
      "index.ts":
        'export default {\n  fetch() {\n    return new Response("Hello from EdgeR");\n  },\n};\n',
    },
  },
  RoutesTable: {
    category: "backend",
    description: "Declarative routes with params and method maps.",
    name: "Routes Table",
    runtime: "EdgeR · Deno",
    supported: true,
    files: {
      "manifest.yaml":
        'name: routes-webide\nversion: "1.0.0"\nentrypoint: index.ts\nkind: routes\n',
      "index.ts":
        'export const routes = {\n  "/": () => new Response("Home"),\n  "/hello/:name": ({ params }) => new Response(`Hello ${params.name}`),\n};\n',
    },
  },
  StaticSpa: {
    category: "frontend",
    description: "Static application served directly by EdgeR.",
    name: "Static SPA",
    runtime: "EdgeR · Static",
    supported: true,
    files: {
      "manifest.yaml":
        'name: static-webide\nversion: "1.0.0"\nentrypoint: index.html\n',
      "index.html":
        '<!doctype html>\n<html lang="en">\n  <head><meta charset="utf-8"><title>EdgeR app</title></head>\n  <body><h1>Hello from EdgeR</h1></body>\n</html>\n',
    },
  },
  React: {
    category: "frontend",
    description: "React 19 SPA using browser-native ESM imports.",
    name: "React",
    runtime: "Static SPA · ESM",
    supported: true,
    files: {
      "manifest.yaml":
        'name: react-webide\nversion: "1.0.0"\nentrypoint: index.html\n',
      "index.html":
        '<!doctype html>\n<html lang="en">\n<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>React on EdgeR</title></head>\n<body><div id="root"></div><script type="module" src="./app.js"></script></body>\n</html>\n',
      "app.js":
        'import React from "https://esm.sh/react@19";\nimport { createRoot } from "https://esm.sh/react-dom@19/client";\n\nfunction App() {\n  return React.createElement("h1", null, "React on EdgeR");\n}\n\ncreateRoot(document.getElementById("root")).render(React.createElement(App));\n',
    },
  },
  Vue: {
    category: "frontend",
    description: "Vue 3 SPA using the browser ESM build.",
    name: "Vue",
    runtime: "Static SPA · ESM",
    supported: true,
    files: {
      "manifest.yaml":
        'name: vue-webide\nversion: "1.0.0"\nentrypoint: index.html\n',
      "index.html":
        '<!doctype html>\n<html lang="en">\n<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Vue on EdgeR</title></head>\n<body><div id="app"></div><script type="module" src="./app.js"></script></body>\n</html>\n',
      "app.js":
        'import { createApp } from "https://esm.sh/vue@3/dist/vue.esm-browser.prod.js";\ncreateApp({ template: `<h1>Vue on EdgeR</h1>` }).mount("#app");\n',
    },
  },
  Svelte: {
    category: "frontend",
    description: "Svelte project with a compile step.",
    name: "Svelte",
    runtime: "Build pipeline required",
    supported: false,
  },
  TanStackStart: {
    category: "fullstack",
    description: "Full-stack React application powered by TanStack Start.",
    name: "TanStack Start",
    runtime: "Fullstack pipeline required",
    supported: false,
  },
  NextJs: {
    category: "fullstack",
    description: "Next.js application with server rendering and routing.",
    name: "Next.js",
    runtime: "Node-compatible pipeline required",
    supported: false,
  },
};

const DB_NAME = "edger-webide";
const DB_VERSION = 2;
const PROJECT_STORE = "projects";
const LEGACY_STORE = "drafts";

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(PROJECT_STORE))
        request.result.createObjectStore(PROJECT_STORE, { keyPath: "id" });
      if (!request.result.objectStoreNames.contains(LEGACY_STORE))
        request.result.createObjectStore(LEGACY_STORE);
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

export async function loadProjects(): Promise<Project[]> {
  const db = await openDb();
  const projects = await new Promise<Project[]>((resolve, reject) => {
    const request = db
      .transaction(PROJECT_STORE)
      .objectStore(PROJECT_STORE)
      .getAll();
    request.onsuccess = () => resolve((request.result ?? []) as Project[]);
    request.onerror = () => reject(request.error);
  });
  return projects
    .map((project) => ({
      ...project,
      folders: project.folders ?? [],
      settings: project.settings ?? {},
    }))
    .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt));
}

export async function saveProject(project: Project) {
  const db = await openDb();
  project.updatedAt = new Date().toISOString();
  await new Promise<void>((resolve, reject) => {
    const transaction = db.transaction(PROJECT_STORE, "readwrite");
    transaction.objectStore(PROJECT_STORE).put(structuredClone(project));
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error);
  });
}

export async function deleteProject(id: string) {
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const transaction = db.transaction(PROJECT_STORE, "readwrite");
    transaction.objectStore(PROJECT_STORE).delete(id);
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error);
  });
  localStorage.removeItem(`edger.webide.workbench.${id}`);
}

export function slugify(value: string) {
  return (
    value
      .toLowerCase()
      .trim()
      .replace(/[^a-z0-9._-]+/g, "-")
      .replace(/^-+|-+$/g, "") || "edger-app"
  );
}

export function parseManifest(text: string) {
  const value = (key: string) =>
    text.match(new RegExp(`^${key}:\\s*["']?([^"'\\n]+)`, "m"))?.[1]?.trim();
  return {
    entrypoint: value("entrypoint"),
    kind: value("kind"),
    name: value("name"),
    version: value("version"),
  };
}

export function validateFilePath(path: string) {
  if (
    !path ||
    path.startsWith("/") ||
    path.includes("\\") ||
    path.split("/").some((part) => !part || part === "." || part === "..")
  ) {
    throw new Error(`Invalid project file path: ${path || "(empty)"}`);
  }
}

export function validateProject(project: Project) {
  const manifest = project.files["manifest.yaml"];
  if (!manifest) throw new Error("manifest.yaml is required");
  const parsed = parseManifest(manifest);
  if (!parsed.name || !parsed.version)
    throw new Error("Manifest name and version are required");
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(parsed.name))
    throw new Error("Worker name must be URL-safe");
  if (!parsed.entrypoint || project.files[parsed.entrypoint] === undefined)
    throw new Error("Manifest entrypoint must exist in the project");
  Object.keys(project.files).forEach(validateFilePath);
  return parsed as Required<ReturnType<typeof parseManifest>>;
}

function makeId() {
  return (
    crypto.randomUUID?.() ??
    `${Date.now()}-${Math.random().toString(16).slice(2)}`
  );
}

export function createProject(
  type: ProjectType,
  requestedName: string,
): Project {
  const template = templates[type];
  if (!template.supported || !template.files)
    throw new Error(`${template.name} is not deployable by EdgeR yet`);
  const name = slugify(requestedName);
  const files = structuredClone(template.files);
  files["manifest.yaml"] = files["manifest.yaml"].replace(
    /^name:.*$/m,
    `name: ${name}`,
  );
  const selected =
    Object.keys(files).find((file) => file !== "manifest.yaml") ??
    "manifest.yaml";
  const now = new Date().toISOString();
  return {
    createdAt: now,
    deployments: [],
    files,
    folders: [],
    id: makeId(),
    logs: [
      {
        at: now,
        level: "info",
        message: `Created ${name} from ${type}.`,
        source: "WEBIDE",
      },
    ],
    name,
    openTabs: [selected],
    previewUrl: "",
    selected,
    settings: {},
    type,
    updatedAt: now,
    version: "1.0.0",
  };
}

export function duplicateProject(source: Project): Project {
  const copy = structuredClone(source);
  const now = new Date().toISOString();
  copy.id = makeId();
  copy.name = `${source.name}-copy`;
  copy.files["manifest.yaml"] = copy.files["manifest.yaml"].replace(
    /^name:.*$/m,
    `name: ${copy.name}`,
  );
  copy.previewUrl = "";
  copy.deployments = [];
  copy.logs = [
    {
      at: now,
      level: "info",
      message: `Duplicated from ${source.name}.`,
      source: "WEBIDE",
    },
  ];
  copy.createdAt = now;
  copy.updatedAt = now;
  return copy;
}

export async function importProject(filesList: FileList): Promise<Project> {
  const selectedFiles = [...filesList];
  if (!selectedFiles.length) throw new Error("Choose a project folder first");
  const firstPath =
    selectedFiles[0].webkitRelativePath || selectedFiles[0].name;
  const root = firstPath.includes("/") ? firstPath.split("/")[0] : "";
  const files: Record<string, string> = {};
  for (const file of selectedFiles) {
    const relative = file.webkitRelativePath || file.name;
    const path =
      root && relative.startsWith(`${root}/`)
        ? relative.slice(root.length + 1)
        : relative;
    if (!path || path.endsWith("/.DS_Store") || path === ".DS_Store") continue;
    validateFilePath(path);
    files[path] = await file.text();
  }
  const manifest = parseManifest(files["manifest.yaml"] ?? "");
  if (!manifest.name || !manifest.version || !manifest.entrypoint)
    throw new Error(
      "Choose a project folder containing a complete manifest.yaml",
    );
  if (files[manifest.entrypoint] === undefined)
    throw new Error(`Imported entrypoint is missing: ${manifest.entrypoint}`);
  const type: ProjectType =
    manifest.kind === "routes"
      ? "RoutesTable"
      : manifest.entrypoint.endsWith(".html")
        ? "StaticSpa"
        : "FetchHandler";
  const now = new Date().toISOString();
  const project: Project = {
    createdAt: now,
    deployments: [],
    files,
    folders: [
      ...new Set(
        Object.keys(files).flatMap((path) =>
          path
            .split("/")
            .slice(0, -1)
            .map((_part, index, parts) => parts.slice(0, index + 1).join("/")),
        ),
      ),
    ],
    id: makeId(),
    logs: [
      {
        at: now,
        level: "info",
        message: `Imported ${manifest.name} from a local folder.`,
        source: "WEBIDE",
      },
    ],
    name: slugify(manifest.name),
    openTabs: [manifest.entrypoint],
    previewUrl: "",
    selected: manifest.entrypoint,
    settings: {},
    type,
    updatedAt: now,
    version: manifest.version,
  };
  validateProject(project);
  return project;
}
