// Persistent Deno worker harness (Epic 15, story 15.A).
//
// Connects to the orchestrator over a Unix domain socket, imports the user
// module ONCE, and serves requests received as length-prefixed JSON frames
// (u32 LE length + UTF-8 JSON). One request at a time per process.

const socketPath = Deno.args[0];
const entryUrl = Deno.args[1];

let conn;

async function writeFrame(payload) {
  const header = new Uint8Array(4);
  new DataView(header.buffer).setUint32(0, payload.length, true);
  await writeAll(header);
  await writeAll(payload);
}

async function writeAll(bytes) {
  let offset = 0;
  while (offset < bytes.length) {
    offset += await conn.write(bytes.subarray(offset));
  }
}

async function readExact(n) {
  const buf = new Uint8Array(n);
  let offset = 0;
  while (offset < n) {
    const read = await conn.read(buf.subarray(offset));
    if (read === null) return null; // EOF
    offset += read;
  }
  return buf;
}

async function readFrame() {
  const header = await readExact(4);
  if (header === null) return null;
  const len = new DataView(header.buffer).getUint32(0, true);
  return await readExact(len);
}

function sendJson(obj) {
  return writeFrame(new TextEncoder().encode(JSON.stringify(obj)));
}

// --- module load + handler capture (mirrors the v1 bridge conventions) ---

let capturedHandler = null;
const originalServe = Deno.serve;
Deno.serve = (arg) => {
  if (typeof arg === "function") {
    capturedHandler = arg;
  } else if (arg && typeof arg.fetch === "function") {
    capturedHandler = arg.fetch.bind(arg);
  } else if (arg && typeof arg.handler === "function") {
    capturedHandler = arg.handler.bind(arg);
  }
  return {
    finished: Promise.resolve(),
    ref() {},
    shutdown() {},
    unref() {},
  };
};

function matchRoutePattern(pattern, pathname) {
  const patternParts = pattern.split("/");
  const pathParts = pathname.split("/");
  const params = {};
  for (let index = 0; index < patternParts.length; index++) {
    const part = patternParts[index];
    if (part === "*") {
      return index === patternParts.length - 1 ? params : null;
    }
    if (part.startsWith(":")) {
      const value = pathParts[index];
      if (value === undefined || value === "") return null;
      params[part.slice(1)] = decodeURIComponent(value);
      continue;
    }
    if (part !== pathParts[index]) return null;
  }
  return patternParts.length === pathParts.length ? params : null;
}

function makeRoutesHandler(routes, fallback) {
  const entries = Object.entries(routes);
  return async (request) => {
    const pathname = new URL(request.url).pathname;
    let target = null;
    let params = null;
    for (const [pattern, value] of entries) {
      if (pattern === pathname) {
        target = value;
        params = {};
        break;
      }
    }
    if (target === null) {
      for (const [pattern, value] of entries) {
        if (!pattern.includes(":") && !pattern.includes("*")) continue;
        const matched = matchRoutePattern(pattern, pathname);
        if (matched) {
          target = value;
          params = matched;
          break;
        }
      }
    }
    if (target === null) {
      if (typeof fallback === "function") return fallback(request);
      return new Response("route not found", { status: 404 });
    }
    if (target && typeof target === "object" && !(target instanceof Response)) {
      target = target[request.method.toUpperCase()];
      if (target === undefined) return new Response("method not allowed", { status: 405 });
    }
    if (target instanceof Response) return target.clone();
    if (typeof target !== "function") {
      throw new Error("invalid routes table entry for " + pathname);
    }
    Object.defineProperty(request, "params", { configurable: true, value: params });
    return target(request);
  };
}

async function loadHandler() {
  const mod = await import(entryUrl);
  Deno.serve = originalServe;
  let handler = capturedHandler;
  if (!handler && mod) {
    if (typeof mod.default === "function") {
      handler = mod.default;
    } else if (mod.default && typeof mod.default.fetch === "function") {
      handler = mod.default.fetch.bind(mod.default);
    } else if (typeof mod.fetch === "function") {
      handler = mod.fetch;
    }
  }
  const routesTable = (mod && ((mod.default && mod.default.routes) || mod.routes)) || null;
  if (routesTable) {
    handler = makeRoutesHandler(routesTable, handler);
  }
  if (typeof handler !== "function") {
    throw new Error("no fetch handler or routes table found in module");
  }
  return handler;
}

function buildRequest(raw) {
  const headers = new Headers(raw.headers ?? []);
  const method = raw.method ?? "GET";
  const init = { method, headers };
  if (raw.body && !["GET", "HEAD"].includes(method.toUpperCase())) {
    init.body = new Uint8Array(raw.body);
  }
  const hasScheme = /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(raw.uri ?? "");
  const url = hasScheme
    ? raw.uri
    : "http://edger.local" + (raw.uri?.startsWith("/") ? raw.uri : "/" + (raw.uri ?? ""));
  return new Request(url, init);
}

async function respond(handler, raw) {
  const response = await handler(buildRequest(raw));
  const bodyBytes = response.body
    ? new Uint8Array(await response.arrayBuffer())
    : new Uint8Array();
  return {
    status: response.status,
    headers: Array.from(response.headers.entries()),
    body: Array.from(bodyBytes),
  };
}

async function main() {
  conn = await Deno.connect({ path: socketPath, transport: "unix" });

  let handler;
  try {
    handler = await loadHandler();
  } catch (err) {
    await sendJson({ ready: false, error: String(err?.stack ?? err) });
    Deno.exit(1);
  }
  await sendJson({ ready: true });

  while (true) {
    const frame = await readFrame();
    if (frame === null) break; // orchestrator closed the connection
    const raw = JSON.parse(new TextDecoder().decode(frame));
    let out;
    try {
      out = await respond(handler, raw);
    } catch (err) {
      out = {
        status: 500,
        headers: [["content-type", "text/plain"]],
        body: Array.from(new TextEncoder().encode(String(err?.stack ?? err))),
      };
    }
    await writeFrame(new TextEncoder().encode(JSON.stringify(out)));
  }
}

main().catch((err) => {
  console.error("harness fatal:", err);
  Deno.exit(1);
});
