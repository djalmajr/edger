import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import { Badge } from "@edger/ui/components/ui/badge";
import { Button } from "@edger/ui/components/ui/button";
import { Card, CardDescription, CardTitle } from "@edger/ui/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@edger/ui/components/ui/dialog";
import { Input } from "@edger/ui/components/ui/input";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
} from "@edger/ui/components/ui/input-group";
import { Label } from "@edger/ui/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@edger/ui/components/ui/table";
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
  AsteriskIcon,
  BoxesIcon,
  BracesIcon,
  ChevronRightIcon,
  CopyIcon,
  FolderInputIcon,
  ImportIcon,
  Layers3Icon,
  LayoutDashboardIcon,
  PanelsTopLeftIcon,
  PencilIcon,
  PlusIcon,
  RouteIcon,
  SearchIcon,
  Trash2Icon,
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

type DashboardProps = { onOpenProject(id: string): void };
type ProjectDialog = { kind: "delete" | "rename"; project: Project } | null;

const CATEGORY_LABELS = {
  frontend: "Frontend",
  backend: "Backend",
  fullstack: "Fullstack",
} as const;

function TemplateIcon({ type }: { type: ProjectType }) {
  const Icon =
    type === "RoutesTable"
      ? RouteIcon
      : type === "FetchHandler"
        ? BracesIcon
        : type === "StaticSpa"
          ? PanelsTopLeftIcon
          : Layers3Icon;
  return <Icon className="size-5" />;
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
  const [search, setSearch] = React.useState("");
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
  const normalizedSearch = search.trim().toLowerCase();
  const filtered = projects.filter(
    (project) =>
      !normalizedSearch ||
      project.name.toLowerCase().includes(normalizedSearch) ||
      project.type.toLowerCase().includes(normalizedSearch),
  );
  const visible = section === "dashboard" ? filtered.slice(0, 6) : filtered;

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

  return (
    <main className="grid min-h-screen grid-cols-[13rem_minmax(0,1fr)] grid-rows-[3.75rem_minmax(0,1fr)] bg-background text-foreground">
      <header className="col-span-2 flex items-center border-b bg-card px-5">
        <div className="flex w-48 items-center gap-3 font-heading font-semibold">
          <AsteriskIcon className="size-6 text-primary dark:text-white" />
          <span>WebIDE</span>
        </div>
        <InputGroup className="mx-auto max-w-md">
          <InputGroupAddon>
            <SearchIcon />
          </InputGroupAddon>
          <InputGroupInput
            aria-label="Search projects"
            onChange={(event) => setSearch(event.currentTarget.value)}
            placeholder="Search projects…"
            value={search}
          />
        </InputGroup>
      </header>

      <aside className="border-r bg-sidebar p-3 text-sidebar-foreground">
        <nav className="flex flex-col gap-1">
          <Button
            className="justify-start"
            onClick={() => setSection("dashboard")}
            variant={section === "dashboard" ? "secondary" : "ghost"}
          >
            <LayoutDashboardIcon /> Dashboard
          </Button>
          <Button
            className="justify-start"
            onClick={() => setSection("projects")}
            variant={section === "projects" ? "secondary" : "ghost"}
          >
            <BoxesIcon /> Projects
          </Button>
        </nav>
      </aside>

      <section className="min-w-0 overflow-auto px-8 py-7 lg:px-12">
        <div className="mx-auto flex max-w-6xl flex-col gap-7">
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
              <Card>
                <Button
                  className="h-auto w-full justify-start gap-4 rounded-xl px-4 py-5 text-left"
                  onClick={() => setTemplateOpen(true)}
                  variant="ghost"
                >
                  <span className="grid size-11 place-items-center rounded-lg bg-primary/15 text-primary">
                    <PlusIcon className="size-6" />
                  </span>
                  <div className="min-w-0 flex-1">
                    <CardTitle>New project</CardTitle>
                    <CardDescription>
                      Choose an EdgeR starter and begin in the workbench.
                    </CardDescription>
                  </div>
                  <ChevronRightIcon />
                </Button>
              </Card>
              <Card>
                <Button
                  className="h-auto w-full justify-start gap-4 rounded-xl px-4 py-5 text-left"
                  onClick={() => importInput.current?.click()}
                  variant="ghost"
                >
                  <span className="grid size-11 place-items-center rounded-lg bg-primary/15 text-primary">
                    <FolderInputIcon className="size-6" />
                  </span>
                  <div className="min-w-0 flex-1">
                    <CardTitle>Import</CardTitle>
                    <CardDescription>
                      Open a local project folder containing manifest.yaml.
                    </CardDescription>
                  </div>
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
                  onChange={(event) =>
                    void importFiles(event.currentTarget.files)
                  }
                />
              </Card>
            </div>
          )}

          <section className="flex flex-col gap-3">
            <h2 className="font-heading text-base font-medium">
              {section === "dashboard" ? "Recent projects" : "All projects"}
            </h2>
            <div className="overflow-hidden rounded-xl border bg-card">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Project</TableHead>
                    <TableHead>Runtime</TableHead>
                    <TableHead>Version</TableHead>
                    <TableHead>Updated</TableHead>
                    <TableHead>
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {visible.map((project) => (
                    <TableRow
                      className="cursor-pointer hover:bg-muted/50"
                      key={project.id}
                      onClick={() => onOpenProject(project.id)}
                      tabIndex={0}
                      onKeyDown={(event) => {
                        if (event.key === "Enter" || event.key === " ")
                          onOpenProject(project.id);
                      }}
                    >
                      <TableCell>
                        <div className="flex items-center gap-3">
                          <span className="grid size-9 place-items-center rounded-lg bg-primary/15 text-primary">
                            <TemplateIcon type={project.type} />
                          </span>
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
                      </TableCell>
                      <TableCell>
                        {templates[project.type]?.name ?? project.type}
                      </TableCell>
                      <TableCell>
                        <Badge variant="secondary" className="font-mono">
                          {project.version}
                        </Badge>
                      </TableCell>
                      <TableCell className="text-muted-foreground">
                        {new Date(project.updatedAt).toLocaleString([], {
                          dateStyle: "medium",
                          timeStyle: "short",
                        })}
                      </TableCell>
                      <TableCell onClick={(event) => event.stopPropagation()}>
                        <div className="flex justify-end gap-1">
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
                      </TableCell>
                    </TableRow>
                  ))}
                  {!projectsQuery.isLoading && visible.length === 0 && (
                    <TableRow>
                      <TableCell
                        colSpan={5}
                        className="h-40 text-center text-muted-foreground"
                      >
                        <ImportIcon className="mx-auto mb-2 size-6" />
                        No projects yet. Create or import one to get started.
                      </TableCell>
                    </TableRow>
                  )}
                </TableBody>
              </Table>
            </div>
          </section>
        </div>
      </section>

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
