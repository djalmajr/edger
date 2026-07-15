import type { Project, ProjectType } from "./projects";

export type ProjectBrand =
  | "deno"
  | "html"
  | "next"
  | "node"
  | "react"
  | "svelte"
  | "vite"
  | "vue";

type ProjectIdentity = Pick<Project, "files" | "type">;

function packageDependencies(files: Record<string, string>) {
  const source = Object.entries(files).find(([path]) =>
    /(^|\/)package\.json$/.test(path),
  )?.[1];
  if (!source) return { dependencies: new Set<string>(), exists: false };

  try {
    const manifest = JSON.parse(source) as Record<string, unknown>;
    const dependencies = new Set<string>();
    for (const field of ["dependencies", "devDependencies", "peerDependencies"]) {
      const entries = manifest[field];
      if (entries && typeof entries === "object")
        Object.keys(entries).forEach((name) => dependencies.add(name));
    }
    return { dependencies, exists: true };
  } catch {
    return { dependencies: new Set<string>(), exists: true };
  }
}

function typeIs(type: ProjectType, ...types: ProjectType[]) {
  return types.includes(type);
}

export function detectProjectBrand(project: ProjectIdentity): ProjectBrand {
  const paths = Object.keys(project.files);
  const { dependencies, exists: hasPackageJson } = packageDependencies(
    project.files,
  );
  const hasDependency = (...names: string[]) =>
    names.some((name) => dependencies.has(name));
  const hasConfig = (pattern: RegExp) => paths.some((path) => pattern.test(path));

  if (typeIs(project.type, "NextJs") || hasDependency("next")) return "next";
  if (
    typeIs(project.type, "Svelte") ||
    hasDependency("svelte", "@sveltejs/kit") ||
    hasConfig(/(^|\/)svelte\.config\.[cm]?[jt]s$/)
  )
    return "svelte";
  if (typeIs(project.type, "Vue") || hasDependency("vue", "nuxt"))
    return "vue";
  if (
    typeIs(project.type, "React", "TanStackStart") ||
    hasDependency("react", "react-dom", "@tanstack/react-start")
  )
    return "react";
  if (
    hasDependency("vite") ||
    hasConfig(/(^|\/)vite\.config\.[cm]?[jt]s$/)
  )
    return "vite";
  if (hasPackageJson) return "node";
  if (typeIs(project.type, "FetchHandler", "RoutesTable")) return "deno";
  return "html";
}
