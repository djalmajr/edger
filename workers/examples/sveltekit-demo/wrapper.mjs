// SvelteKit on EdgeR (story 16.B recipe, revised after live browser testing):
// the build carries `paths.base = /sveltekit-demo` so client assets resolve
// absolutely under the worker mount (relative paths break on the bare `/name`
// URL). EdgeR strips the base before dispatch, so this wrapper restores it
// (via the `x-base` header) before delegating to the exported handler.
import http from "node:http";
import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import process from "node:process";
import { handler } from "./handler.js";

// EdgeR may bundle Node-compatible entrypoints into a temporary directory.
// The worker process cwd remains the declared worker root, where client assets
// live, so asset resolution must not be relative to import.meta.url.
const clientDir = path.join(process.cwd(), "client");
const contentTypes = new Map([
  [".css", "text/css; charset=utf-8"],
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".svg", "image/svg+xml"],
  [".txt", "text/plain; charset=utf-8"],
  [".webp", "image/webp"],
]);

http
  .createServer(async (req, res) => {
    const base = req.headers["x-base"] ?? "";
    if (base && !req.url.startsWith(`${base}/`) && req.url !== base) {
      req.url = base + (req.url === "/" ? "/" : req.url);
    }

    const pathname = decodeURIComponent(new URL(req.url, "http://edger").pathname);
    const assetPath = path.resolve(clientDir, `.${pathname}`);
    if (assetPath.startsWith(`${clientDir}${path.sep}`)) {
      try {
        if ((await stat(assetPath)).isFile()) {
          const body = await readFile(assetPath);
          res.statusCode = 200;
          res.setHeader(
            "content-type",
            contentTypes.get(path.extname(assetPath)) ?? "application/octet-stream",
          );
          if (pathname.includes("/_app/immutable/")) {
            res.setHeader("cache-control", "public, max-age=31536000, immutable");
          }
          res.end(req.method === "HEAD" ? undefined : body);
          return;
        }
      } catch (error) {
        if (error?.code !== "ENOENT") {
          console.error("static asset read failed", error);
        }
      }
    }

    handler(req, res, () => {
      res.statusCode = 404;
      res.end("not found");
    });
  })
  .listen(3000);
