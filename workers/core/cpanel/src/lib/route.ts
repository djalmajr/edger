// Pure route helpers for the cPanel SPA. The mount prefix comes from the
// runtime-injected <base href> — the proxied prefix behind a stripping
// proxy, or the bare fallback when the page has no base — so no path is
// hardcoded and nothing here touches window/document/location.
export type View =
  | "overview"
  | "workers"
  | "observability"
  | "logs"
  | "files"
  | "keys";
export type Target = { name: string; version: string };
export type RouteState = { path: string; target?: Target; view: View };

export function cpanelBasePath(workerBasePath: string): string {
  return workerBasePath || "/cpanel";
}

// Parses `pathname` relative to `basePath` (no trailing slash, e.g.
// "/apps/cpanel"). Paths equal to or under `basePath + "/"` are interpreted
// with the SPA's view rules; anything else resolves to the overview.
export function readRoute(pathname: string, basePath: string): RouteState {
  const inside =
    pathname === basePath || pathname.startsWith(basePath + "/");
  if (!inside) return { path: "", view: "overview" };
  const parts = pathname.slice(basePath.length).split("/").filter(Boolean);
  if (parts[0] === "keys") return { path: "", view: "keys" };
  if (parts[0] === "observability")
    return { path: "", view: parts[1] === "logs" ? "logs" : "observability" };
  if (parts[0] !== "workers") return { path: "", view: "overview" };
  if (
    parts.length < 4 ||
    !["files", "logs", "observability"].includes(parts[3])
  )
    return { path: "", view: "workers" };
  return {
    path:
      parts[3] === "files"
        ? parts.slice(4).map(decodeURIComponent).join("/")
        : "",
    target: {
      name: decodeURIComponent(parts[1]),
      version: decodeURIComponent(parts[2]),
    },
    view: parts[3] as View,
  };
}

// Builds the SPA URL for `route` under `basePath` (no trailing slash),
// encoding worker name, version and file-path segments.
export function routePath(route: RouteState, basePath: string): string {
  if (route.view === "overview") return `${basePath}/`;
  if (route.view === "workers" && !route.target) return `${basePath}/workers`;
  if (route.view === "keys") return `${basePath}/keys`;
  if (route.view === "observability" && !route.target)
    return `${basePath}/observability`;
  if (route.view === "logs" && !route.target)
    return `${basePath}/observability/logs`;
  if (!route.target) return `${basePath}/workers`;
  const suffix =
    route.view === "files" && route.path
      ? `/${route.path.split("/").map(encodeURIComponent).join("/")}`
      : "";
  return `${basePath}/workers/${encodeURIComponent(route.target.name)}/${encodeURIComponent(route.target.version)}/${route.view}${suffix}`;
}
