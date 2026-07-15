import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  type ColumnDef,
  getCoreRowModel,
  getPaginationRowModel,
  useReactTable,
} from "@tanstack/react-table";
import * as React from "react";
import DenoLogo from "~icons/logos/deno";
import HtmlLogo from "~icons/logos/html-5";
import NextLogo from "~icons/logos/nextjs-icon";
import NodeLogo from "~icons/logos/nodejs-icon";
import ReactLogo from "~icons/logos/react";
import SvelteLogo from "~icons/logos/svelte-icon";
import ViteLogo from "~icons/logos/vitejs";
import VueLogo from "~icons/logos/vue";

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
import { Input } from "@edger/ui/components/ui/input";
import { Label } from "@edger/ui/components/ui/label";
import { ScrollArea } from "@edger/ui/components/ui/scroll-area";
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "@edger/ui/components/ui/tabs";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@edger/ui/components/ui/tooltip";
import {
  AtomIcon,
  BoxesIcon,
  BracesIcon,
  ChevronRightIcon,
  CopyIcon,
  FolderInputIcon,
  FlameIcon,
  ImportIcon,
  LayoutDashboardIcon,
  PanelsTopLeftIcon,
  PencilIcon,
  PlusIcon,
  RouteIcon,
  ComponentIcon,
  NetworkIcon,
  Trash2Icon,
  TriangleIcon,
  WebhookIcon,
} from "@edger/ui/icons/lucide";

import {
  createProject,
  deleteProject,
  duplicateProject,
  importProject,
  loadProjects,
  type Project,
  type ProjectType,
  saveProject,
  slugify,
  templates,
} from "../lib/projects";
import {
  detectProjectBrand,
  type ProjectBrand,
} from "../lib/project-brand";
import { DataTable } from "./data-table/data-table";

type DashboardProps = { onOpenProject(id: string): void };
type ProjectDialog = { kind: "delete" | "rename"; project: Project } | null;

const CATEGORY_LABELS = {
  frontend: "Frontend",
  backend: "Backend",
  fullstack: "Fullstack",
} as const;

const PROJECT_TYPE_ICONS: Record<
  ProjectType,
  React.ComponentType<{ className?: string }>
> = {
  FetchHandler: BracesIcon,
  NextJs: TriangleIcon,
  React: AtomIcon,
  RoutesTable: RouteIcon,
  StaticSpa: PanelsTopLeftIcon,
  Svelte: FlameIcon,
  TanStackStart: NetworkIcon,
  Vue: ComponentIcon,
};

function TemplateIcon({ type }: { type: ProjectType }) {
  const Icon = PROJECT_TYPE_ICONS[type];
  return <Icon className="size-5" />;
}

const PROJECT_BRAND_LOGOS: Record<
  ProjectBrand,
  {
    className: string;
    icon: React.ComponentType<{ className?: string }>;
    label: string;
  }
> = {
  deno: { className: "size-6 dark:invert", icon: DenoLogo, label: "Deno" },
  html: { className: "size-6", icon: HtmlLogo, label: "HTML" },
  next: {
    className: "size-6 dark:invert",
    icon: NextLogo,
    label: "Next.js",
  },
  node: { className: "size-6", icon: NodeLogo, label: "Node.js" },
  react: { className: "size-6", icon: ReactLogo, label: "React" },
  svelte: { className: "size-6", icon: SvelteLogo, label: "Svelte" },
  vite: { className: "size-6", icon: ViteLogo, label: "Vite" },
  vue: { className: "size-6", icon: VueLogo, label: "Vue" },
};

function ProjectLogo({ project }: { project: Project }) {
  const brand = PROJECT_BRAND_LOGOS[detectProjectBrand(project)];
  const Icon = brand.icon;

  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <span
            aria-label={`${brand.label} project`}
            className="grid size-9 place-items-center rounded-lg bg-muted/60"
            role="img"
          />
        }
      >
        <Icon className={brand.className} />
      </TooltipTrigger>
      <TooltipContent>{brand.label}</TooltipContent>
    </Tooltip>
  );
}

function IconButton({
  label,
  children,
  ...props
}: React.ComponentProps<typeof Button> & { label: string }) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <Button
            aria-label={label}
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

export function Dashboard({ onOpenProject }: DashboardProps) {
  const queryClient = useQueryClient();
  const projectsQuery = useQuery({
    queryKey: ["webide", "projects"],
    queryFn: loadProjects,
  });
  const [section, setSection] = React.useState<"dashboard" | "projects">(
    "dashboard",
  );
  const [templateOpen, setTemplateOpen] = React.useState(false);
  const [templateCategory, setTemplateCategory] =
    React.useState<keyof typeof CATEGORY_LABELS>("frontend");
  const [projectDialog, setProjectDialog] = React.useState<ProjectDialog>(null);
  const [projectName, setProjectName] = React.useState("");
  const [error, setError] = React.useState("");
  const importInput = React.useRef<HTMLInputElement>(null);

  const refresh = React.useCallback(
    () => queryClient.invalidateQueries({ queryKey: ["webide", "projects"] }),
    [queryClient],
  );
  const mutation = useMutation({
    mutationFn: async (operation: () => Promise<void>) => operation(),
    onSuccess: async () => {
      setError("");
      setProjectDialog(null);
      await refresh();
    },
    onError: (reason) =>
      setError(reason instanceof Error ? reason.message : String(reason)),
  });

  const projects = projectsQuery.data ?? [];
  function nextProjectName(type: ProjectType) {
    const base = slugify(`${templates[type].name}-app`);
    const names = new Set(projects.map((project) => project.name));
    if (!names.has(base)) return base;
    let suffix = 2;
    while (names.has(`${base}-${suffix}`)) suffix += 1;
    return `${base}-${suffix}`;
  }

  async function create(type: ProjectType) {
    try {
      const project = createProject(type, nextProjectName(type));
      await saveProject(project);
      setTemplateOpen(false);
      await refresh();
      onOpenProject(project.id);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }

  async function importFiles(files: FileList | null) {
    if (!files?.length) return;
    try {
      const project = await importProject(files);
      await saveProject(project);
      await refresh();
      onOpenProject(project.id);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (importInput.current) importInput.current.value = "";
    }
  }

  function openDialog(kind: "delete" | "rename", project: Project) {
    setError("");
    setProjectName(project.name);
    setProjectDialog({ kind, project });
  }

  function applyProjectDialog() {
    if (!projectDialog) return;
    if (projectDialog.kind === "delete") {
      mutation.mutate(() => deleteProject(projectDialog.project.id));
      return;
    }
    mutation.mutate(async () => {
      const name = slugify(projectName);
      if (!name) throw new Error("Project name is required.");
      const project = structuredClone(projectDialog.project);
      project.name = name;
      project.files["manifest.yaml"] = project.files["manifest.yaml"].replace(
        /^name:.*$/m,
        `name: ${name}`,
      );
      await saveProject(project);
    });
  }

  const projectColumns: ColumnDef<Project>[] = [
    {
      accessorKey: "name",
      header: "Project",
      cell: ({ row }) => {
        const project = row.original;
        return (
          <div className="flex items-center gap-3">
            <ProjectLogo project={project} />
            <span className="min-w-0">
              <strong className="block truncate font-medium">
                {project.name}
              </strong>
              <small className="block truncate text-muted-foreground">
                {project.previewUrl
                  ? `Deployed at ${project.previewUrl}`
                  : "Local draft"}
              </small>
            </span>
          </div>
        );
      },
    },
    {
      accessorKey: "type",
      header: "Runtime",
      cell: ({ row }) => templates[row.original.type].name,
    },
    {
      accessorKey: "version",
      header: "Version",
      cell: ({ row }) => (
        <Badge className="font-mono" variant="secondary">
          {row.original.version}
        </Badge>
      ),
    },
    {
      accessorKey: "updatedAt",
      header: "Updated",
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {new Date(row.original.updatedAt).toLocaleString([], {
            dateStyle: "medium",
            timeStyle: "short",
          })}
        </span>
      ),
    },
    {
      id: "actions",
      header: () => <span className="sr-only">Actions</span>,
      cell: ({ row }) => {
        const project = row.original;
        return (
          <div
            className="flex justify-end gap-1"
            onClick={(event) => event.stopPropagation()}
            onKeyDown={(event) => event.stopPropagation()}
          >
            <IconButton
              label={`Duplicate ${project.name}`}
              onClick={() =>
                mutation.mutate(async () =>
                  saveProject(duplicateProject(project)),
                )
              }
            >
              <CopyIcon />
            </IconButton>
            <IconButton
              label={`Rename ${project.name}`}
              onClick={() => openDialog("rename", project)}
            >
              <PencilIcon />
            </IconButton>
            <IconButton
              label={`Delete ${project.name}`}
              onClick={() => openDialog("delete", project)}
            >
              <Trash2Icon />
            </IconButton>
          </div>
        );
      },
    },
  ];
  const projectTable = useReactTable({
    columns: projectColumns,
    data: projects,
    getCoreRowModel: getCoreRowModel(),
    getPaginationRowModel: getPaginationRowModel(),
    initialState: {
      pagination: {
        pageIndex: 0,
        pageSize: 10,
      },
    },
  });

  return (
    <main className="grid min-h-screen grid-cols-[13rem_minmax(0,1fr)] grid-rows-[2.5rem_minmax(0,1fr)] bg-background text-foreground">
      <header className="col-span-2 flex items-center border-b bg-card px-5">
        <div className="flex items-center gap-3 font-heading font-semibold">
          <WebhookIcon className="size-6 text-primary dark:text-white" />
          <span>WebIDE</span>
        </div>
      </header>

      <aside className="border-r bg-sidebar p-3 text-sidebar-foreground">
        <nav className="flex flex-col gap-1">
          <Button
            className="justify-start"
            onClick={() => {
              projectTable.setPageIndex(0);
              setSection("dashboard");
            }}
            variant={section === "dashboard" ? "secondary" : "ghost"}
          >
            <LayoutDashboardIcon /> Dashboard
          </Button>
          <Button
            className="justify-start"
            onClick={() => {
              projectTable.setPageIndex(0);
              setSection("projects");
            }}
            variant={section === "projects" ? "secondary" : "ghost"}
          >
            <BoxesIcon /> Projects
          </Button>
        </nav>
      </aside>

      <ScrollArea className="min-h-0 min-w-0">
        <div className="mx-auto flex max-w-6xl flex-col gap-7 px-8 py-7 lg:px-12">
          <div>
            <h1 className="font-heading text-4xl font-semibold tracking-tight">
              {section === "dashboard" ? "Build at the edge" : "Projects"}
            </h1>
            <p className="mt-1 text-muted-foreground">
              {section === "dashboard"
                ? "Create, edit, deploy, and inspect EdgeR workers from one workspace."
                : "Local drafts stay in this browser until you deploy explicitly."}
            </p>
          </div>

          {error && (
            <p
              role="alert"
              className="rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive"
            >
              {error}
            </p>
          )}

          {section === "dashboard" && (
            <div className="grid gap-3 md:grid-cols-2">
              <Button
                className="h-auto min-h-28 w-full justify-start gap-5 rounded-xl px-5 py-6 text-left"
                onClick={() => setTemplateOpen(true)}
                variant="outline"
              >
                <span className="grid size-12 place-items-center rounded-lg bg-primary/15 text-primary">
                  <PlusIcon className="size-6" />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block font-heading text-base font-medium text-foreground">
                    New project
                  </span>
                  <span className="mt-1 block text-sm text-muted-foreground">
                    Choose an EdgeR starter and begin in the workbench.
                  </span>
                </span>
                <ChevronRightIcon />
              </Button>
              <Button
                className="h-auto min-h-28 w-full justify-start gap-5 rounded-xl px-5 py-6 text-left"
                onClick={() => importInput.current?.click()}
                variant="outline"
              >
                <span className="grid size-12 place-items-center rounded-lg bg-primary/15 text-primary">
                  <FolderInputIcon className="size-6" />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block font-heading text-base font-medium text-foreground">
                    Import
                  </span>
                  <span className="mt-1 block text-sm text-muted-foreground">
                    Open a local project folder containing manifest.yaml.
                  </span>
                </span>
                <ChevronRightIcon />
              </Button>
              <Input
                className="hidden"
                ref={importInput}
                type="file"
                multiple
                {...({
                  webkitdirectory: "",
                  directory: "",
                } as React.InputHTMLAttributes<HTMLInputElement>)}
                onChange={(event) => void importFiles(event.currentTarget.files)}
              />
            </div>
          )}

          <section className="flex flex-col gap-3">
            {section === "dashboard" ? (
              <h2 className="font-heading text-base font-medium">
                Recent projects
              </h2>
            ) : null}
            <DataTable
              emptyState={
                projectsQuery.isLoading ? (
                  "Loading projects…"
                ) : (
                  <span className="flex flex-col items-center gap-2">
                    <ImportIcon className="size-6" />
                    No projects yet. Create or import one to get started.
                  </span>
                )
              }
              onRowClick={(row) => onOpenProject(row.original.id)}
              paginated={section === "projects"}
              table={projectTable}
            />
          </section>
        </div>
      </ScrollArea>

      <Dialog open={templateOpen} onOpenChange={setTemplateOpen}>
        <DialogContent className="sm:max-w-3xl">
          <DialogHeader>
            <DialogTitle>Create a new project</DialogTitle>
            <DialogDescription>
              Choose a starter that matches what you want to deploy on EdgeR.
            </DialogDescription>
          </DialogHeader>
          <Tabs
            value={templateCategory}
            onValueChange={(value) =>
              setTemplateCategory(value as keyof typeof CATEGORY_LABELS)
            }
          >
            <TabsList className="w-full">
              {Object.entries(CATEGORY_LABELS).map(([value, label]) => (
                <TabsTrigger key={value} value={value}>
                  {label}
                </TabsTrigger>
              ))}
            </TabsList>
            {Object.keys(CATEGORY_LABELS).map((category) => (
              <TabsContent
                className="grid gap-2 pt-2 sm:grid-cols-2"
                key={category}
                value={category}
              >
                {(
                  Object.entries(templates) as [
                    ProjectType,
                    (typeof templates)[ProjectType],
                  ][]
                )
                  .filter(([, template]) => template.category === category)
                  .map(([type, template]) => (
                    <Button
                      className="h-auto justify-start gap-3 p-4 text-left"
                      disabled={!template.supported}
                      key={type}
                      onClick={() => void create(type)}
                      variant="outline"
                    >
                      <span className="grid size-10 place-items-center rounded-lg bg-primary/15 text-primary">
                        <TemplateIcon type={type} />
                      </span>
                      <span className="min-w-0 flex-1">
                        <strong className="block">{template.name}</strong>
                        <small className="block truncate font-normal text-muted-foreground">
                          {template.runtime}
                        </small>
                      </span>
                      <Badge
                        variant={template.supported ? "secondary" : "outline"}
                      >
                        {template.supported ? "Ready" : "Planned"}
                      </Badge>
                    </Button>
                  ))}
              </TabsContent>
            ))}
          </Tabs>
        </DialogContent>
      </Dialog>

      <Dialog
        open={Boolean(projectDialog)}
        onOpenChange={(open) => {
          if (!open) setProjectDialog(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {projectDialog?.kind === "delete"
                ? "Delete project"
                : "Rename project"}
            </DialogTitle>
            <DialogDescription>
              {projectDialog?.kind === "delete"
                ? `Delete the local draft ${projectDialog.project.name}? Deployed workers are not removed.`
                : "Choose a URL-safe name for this project."}
            </DialogDescription>
          </DialogHeader>
          {projectDialog?.kind === "rename" && (
            <div className="grid gap-2">
              <Label htmlFor="rename-project">Project name</Label>
              <Input
                id="rename-project"
                autoFocus
                value={projectName}
                onChange={(event) => setProjectName(event.currentTarget.value)}
              />
            </div>
          )}
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setProjectDialog(null)}>
              Cancel
            </Button>
            <Button
              disabled={mutation.isPending}
              variant={
                projectDialog?.kind === "delete" ? "destructive" : "default"
              }
              onClick={applyProjectDialog}
            >
              {projectDialog?.kind === "delete" ? "Delete" : "Rename"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </main>
  );
}
