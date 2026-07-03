// SvelteKit on EdgeR (story 16.B recipe, revised after live browser testing):
// the build carries `paths.base = /sveltekit-demo` so client assets resolve
// absolutely under the worker mount (relative paths break on the bare `/name`
// URL). EdgeR strips the base before dispatch, so this wrapper restores it
// (via the `x-base` header) before delegating to the exported handler.
import http from "node:http";
import { handler } from "./handler.js";

http
  .createServer((req, res) => {
    const base = req.headers["x-base"] ?? "";
    if (base && !req.url.startsWith(`${base}/`) && req.url !== base) {
      req.url = base + (req.url === "/" ? "/" : req.url);
    }
    handler(req, res, () => {
      res.statusCode = 404;
      res.end("not found");
    });
  })
  .listen(3000);
