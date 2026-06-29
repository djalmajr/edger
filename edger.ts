#!/usr/bin/env bun
/**
 * edger - minimal Bun-based edge runtime loader
 * Supports worker dirs with index.{ts,js,mjs} that are compatible with
 * Deno.serve(...) or export default { fetch(req) {} } patterns from edge-runtime/examples.
 *
 * Usage:
 *   bun edger.ts --dir workers/hello-world --port 8000
 *
 * The worker dir is served at root. Copy examples verbatim into workers/<name>/
 */

import { resolve as pathResolve } from "node:path";

/**
 * Pure adapter: given a worker dir, returns a fetch handler compatible with the examples.
 * Supports Deno.serve shim and default export { fetch }.
 * This is the core logic exercised by tests.
 */
export async function loadWorkerHandler(workerDir: string): Promise<(req: Request) => Response | Promise<Response>> {
  const candidates = [
    `${workerDir}/index.ts`,
    `${workerDir}/index.js`,
    `${workerDir}/index.mjs`,
  ];

  let indexPath: string | null = null;
  for (const p of candidates) {
    if (await Bun.file(p).exists()) {
      indexPath = p;
      break;
    }
  }

  if (!indexPath) {
    throw new Error(`No index.{ts,js,mjs} found in ${workerDir}`);
  }

  indexPath = pathResolve(process.cwd(), indexPath);

  let capturedHandler: ((req: Request) => Response | Promise<Response>) | null = null;
  const origDeno = (globalThis as any).Deno;

  // Shim for module eval time (capture Deno.serve) and provide common Deno APIs
  // used inside handlers (e.g. Deno.readTextFile for serve-html style examples).
  const runtimeDenoShim = {
    ...(origDeno || {}),
    serve: (arg: any) => {
      if (typeof arg === "function") {
        capturedHandler = arg;
      } else if (arg && typeof arg.fetch === "function") {
        capturedHandler = arg.fetch;
      } else if (arg && arg.handler && typeof arg.handler === "function") {
        capturedHandler = arg.handler;
      }
    },
    readTextFile: async (path: string | URL): Promise<string> => {
      return await Bun.file(path).text();
    },
  };

  (globalThis as any).Deno = runtimeDenoShim;

  const mod = await import(`file://${indexPath}`);

  (globalThis as any).Deno = origDeno;

  let handler = capturedHandler;

  if (!handler && mod) {
    if (typeof mod.default === "function") {
      handler = mod.default;
    } else if (mod.default && typeof mod.default.fetch === "function") {
      handler = mod.default.fetch;
    } else if (typeof mod.fetch === "function") {
      handler = mod.fetch;
    } else if (mod.default && typeof mod.default.default === "function") {
      handler = mod.default.default;
    }
  }

  if (typeof handler !== "function") {
    handler = async (_req: Request) =>
      new Response(JSON.stringify({ error: "edger: no fetch handler found in module" }), {
        status: 500,
        headers: { "content-type": "application/json" },
      });
  }

  // Wrap to ensure runtime Deno shims (readTextFile etc) are present when handler
  // body executes (for examples using Deno.* inside fetch, verbatim compat).
  const runtimeShim = {
    ...( (globalThis as any).Deno || {} ),
    readTextFile: async (path: string | URL): Promise<string> => Bun.file(path).text(),
  };
  const wrappedHandler = async (req: Request) => {
    const prev = (globalThis as any).Deno;
    (globalThis as any).Deno = runtimeShim;
    try {
      return await handler(req);
    } finally {
      (globalThis as any).Deno = prev;
    }
  };

  return wrappedHandler;
}

const { values } = Bun.argv.slice(2).reduce((acc: any, cur: string, i: number, arr: string[]) => {
  if (cur.startsWith("--dir") || cur === "-d") {
    acc.values.dir = arr[i + 1];
  } else if (cur.startsWith("--port") || cur === "-p") {
    acc.values.port = arr[i + 1];
  }
  return acc;
}, { values: {} as any });

if (import.meta.main) {
  const workerDir = values.dir || "./workers/hello-world";
  const port = parseInt(values.port || "8000", 10);

  console.log(`[edger] loading worker dir: ${workerDir}`);

  const handler = await loadWorkerHandler(workerDir);

  console.log(`[edger] serving ${workerDir} on http://localhost:${port}`);

  Bun.serve({
    port,
    fetch: async (req: Request) => {
      try {
        return await handler(req);
      } catch (err) {
        console.error("[edger] handler error", err);
        return new Response("edger internal error", { status: 500 });
      }
    },
  });
}
