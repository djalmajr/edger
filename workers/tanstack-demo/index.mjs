// TanStack Start on EdgeR (story 16.C recipe): the build's server bundle is a
// pure fetch handler (createStartHandler) that does NOT serve client assets —
// this thin wrapper serves known client assets statically and delegates the rest.
// Build recipe: vite.config.ts with `ssr: { noExternal: true }` so the server
// bundle is self-contained (Deno resolves only node builtins, no node_modules).
import server from "./server/server.js";

// createStartHandler exports either a callable or a `{ fetch }` object
// depending on the version — support both.
const handler = typeof server === "function" ? server : server.fetch.bind(server);

const TYPES = {
  ".js": "text/javascript",
  ".mjs": "text/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
  ".png": "image/png",
  ".woff2": "font/woff2",
  ".json": "application/json",
};

const PUBLIC_FILES = new Set([
  "/favicon.ico",
  "/logo192.png",
  "/logo512.png",
  "/manifest.json",
  "/robots.txt",
]);

function validateStaticPath(pathname) {
  let decoded;
  try {
    decoded = decodeURIComponent(pathname);
  } catch {
    return { response: new Response("malformed path", { status: 400 }) };
  }

  if (
    decoded.includes("\0") ||
    decoded.includes("..") ||
    decoded.startsWith("//") ||
    decoded.startsWith("/.")
  ) {
    return { response: new Response("not found", { status: 404 }) };
  }

  if (!decoded.startsWith("/")) {
    return { response: new Response("malformed path", { status: 400 }) };
  }

  if (decoded.startsWith("/assets/") || PUBLIC_FILES.has(decoded)) {
    return { pathname: decoded };
  }

  return {};
}

async function tryStatic(pathname) {
  const validated = validateStaticPath(pathname);
  if (validated.response) return validated.response;
  if (!validated.pathname) return null;

  try {
    const data = await Deno.readFile(
      new URL(`./client${validated.pathname}`, import.meta.url),
    );
    const ext = validated.pathname.slice(validated.pathname.lastIndexOf("."));
    return new Response(data, {
      headers: {
        "content-type": TYPES[ext] ?? "application/octet-stream",
        "cache-control": validated.pathname.startsWith("/assets/")
          ? "public, max-age=31536000, immutable"
          : "no-cache",
      },
    });
  } catch {
    return null;
  }
}

// EdgeR mounts workers Buntime-style: the worker receives the path RELATIVE to
// its base and the mount itself in the `x-base` header. The TanStack build has
// `basepath: /tanstack-demo` baked in, so the router expects FULL paths — the
// wrapper restores the base before delegating (and keeps the relative path for
// static lookup).
Deno.serve(async (req) => {
  const url = new URL(req.url);
  const relative = url.pathname;

  if (relative !== "/" && !relative.startsWith("/api/")) {
    const asset = await tryStatic(relative);
    if (asset) return asset;
  }

  const base = req.headers.get("x-base") ?? "";
  const fullPath = base && !relative.startsWith(`${base}/`) && relative !== base
    ? base + (relative === "/" ? "/" : relative)
    : relative;
  const routed = new Request(new URL(fullPath + url.search, url.origin), req);
  return handler(routed);
});
