// Persistent Deno worker harness (Epic 15, story 15.A).
//
// Connects to the orchestrator over a Unix domain socket, imports the user
// module ONCE, and serves requests received as length-prefixed JSON frames
// (u32 LE length + UTF-8 JSON). One request at a time per process.

const socketPath = Deno.args[0];
const entryUrl = Deno.args[1];

// A durable per-worker process must survive background errors thrown by user
// code AFTER a response was sent — a stray `setInterval` that enqueues into a
// cancelled stream, a floating promise rejection, etc. Without these handlers
// such an error is uncaught and Deno terminates the process, breaking the next
// request with a broken-pipe write (surfaced live on the `sse` worker). We log
// them to stderr for diagnostics but keep the process alive; a failing REQUEST
// is still caught per-dispatch in `respond()` and returned as a 500.
globalThis.addEventListener?.("unhandledrejection", (event) => {
  event.preventDefault();
  console.error("worker unhandledrejection:", event.reason);
});
globalThis.addEventListener?.("error", (event) => {
  event.preventDefault?.();
  console.error("worker uncaught error:", event.error ?? event.message);
});

// --- Graceful shutdown lifecycle (story 20.11) ---
// EdgeRuntime.waitUntil(promise) lets `beforeunload` handlers extend the process
// just long enough to drain background work (e.g. a DB pool). On a shutdown
// control frame the orchestrator sends a grace budget; we fire `beforeunload`,
// await the collected promises up to that budget, then ack and exit.
const pendingWaitUntil = [];
globalThis.EdgeRuntime = globalThis.EdgeRuntime ?? {};
globalThis.EdgeRuntime.waitUntil = (promise) => {
  const tracked = Promise.resolve(promise);
  pendingWaitUntil.push(tracked);
  return tracked;
};

async function handleShutdown(reason, graceMs) {
  try {
    globalThis.dispatchEvent(new CustomEvent("beforeunload", { detail: { reason } }));
  } catch (_) {
    // no listeners / dispatch unsupported — nothing to drain.
  }
  const pending = pendingWaitUntil.splice(0);
  if (pending.length > 0 && graceMs > 0) {
    await Promise.race([
      Promise.allSettled(pending),
      new Promise((resolve) => setTimeout(resolve, graceMs)),
    ]);
  }
  return pending.length;
}

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
const capturedNodeHandlers = [];
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

// Node compat: capture a framework's `http.createServer(app).listen()` so
// Express (and other node:http servers) run without actually binding a port.
async function installNodeHttpAdapter() {
  try {
    const nodeHttp = await import("node:http");
    const http = nodeHttp.default ?? nodeHttp;
    http.createServer = (...args) => {
      const listener = args.find((arg) => typeof arg === "function");
      if (listener) capturedNodeHandlers.push(listener);
      // Frameworks register handlers two ways: as the createServer argument
      // (Express: `createServer(app)`) or later via the request event (polka /
      // SvelteKit adapter-node: `createServer()` then `server.on("request", h)`).
      // Node invokes EVERY "request" listener, and frameworks rely on that —
      // adapter-node registers polka's handler AND a shutdown-tracking listener;
      // keeping only the last one would dispatch to a listener that never
      // responds (and the pending await would let the event loop drain, killing
      // the process with exit 0). Capture them all.
      const captureEvent = (event, handler) => {
        if (event === "request" && typeof handler === "function") {
          capturedNodeHandlers.push(handler);
        }
      };
      return {
        address() {
          return { address: "127.0.0.1", family: "IPv4", port: 0 };
        },
        close(callback) {
          if (typeof callback === "function") callback();
          return this;
        },
        listen(...listenArgs) {
          const callback = listenArgs.find((arg) => typeof arg === "function");
          if (callback) callback();
          return this;
        },
        on(event, handler) {
          captureEvent(event, handler);
          return this;
        },
        once(event, handler) {
          captureEvent(event, handler);
          return this;
        },
        addListener(event, handler) {
          captureEvent(event, handler);
          return this;
        },
        removeListener() { return this; },
        setTimeout() { return this; },
        ref() { return this; },
        unref() { return this; },
      };
    };
  } catch (_) {
    // Non-Node workers do not need the server-listen adapter.
  }
}

function asUint8Array(chunk) {
  if (chunk instanceof Uint8Array) return chunk;
  if (chunk instanceof ArrayBuffer) return new Uint8Array(chunk);
  return new TextEncoder().encode(String(chunk));
}

function rawHeadersFrom(headers) {
  const rawHeaders = [];
  for (const [name, value] of headers.entries()) rawHeaders.push(name, value);
  return rawHeaders;
}

async function createNodeRequest(request) {
  const { Readable } = await import("node:stream");
  const url = new URL(request.url);
  const body = request.body ? Readable.fromWeb(request.body) : Readable.from([]);
  body.method = request.method;
  body.url = url.pathname + url.search;
  body.headers = Object.fromEntries(request.headers.entries());
  // `Request` drops forbidden headers like Host, but node servers (e.g.
  // SvelteKit's getRequest) need one to reconstruct the origin — 400 otherwise.
  if (!body.headers.host) body.headers.host = url.host;
  body.rawHeaders = rawHeadersFrom(request.headers);
  body.socket = { encrypted: url.protocol === "https:", remoteAddress: "127.0.0.1" };
  body.connection = body.socket;
  return body;
}

function applyNodeHeaders(target, headers) {
  if (!headers) return;
  if (headers instanceof Headers) {
    for (const [name, value] of headers.entries()) target.set(name, value);
    return;
  }
  if (Array.isArray(headers)) {
    for (let index = 0; index < headers.length; index += 2) {
      target.set(String(headers[index]), String(headers[index + 1] ?? ""));
    }
    return;
  }
  for (const [name, value] of Object.entries(headers)) {
    if (Array.isArray(value)) target.set(name, value.map(String).join(", "));
    else if (value !== undefined) target.set(name, String(value));
  }
}

function concatChunks(chunks, total) {
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
}

function createNodeResponse(resolve) {
  const headers = new Headers();
  const chunks = [];
  let total = 0;
  return {
    statusCode: 200,
    statusMessage: "OK",
    headersSent: false,
    writableEnded: false,
    setHeader(name, value) {
      applyNodeHeaders(headers, { [name]: value });
      return this;
    },
    getHeader(name) { return headers.get(String(name)); },
    hasHeader(name) { return headers.has(String(name)); },
    removeHeader(name) { headers.delete(String(name)); return this; },
    writeHead(statusCode, reasonOrHeaders, maybeHeaders) {
      this.statusCode = statusCode;
      if (typeof reasonOrHeaders === "string") {
        this.statusMessage = reasonOrHeaders;
        applyNodeHeaders(headers, maybeHeaders);
      } else {
        applyNodeHeaders(headers, reasonOrHeaders);
      }
      this.headersSent = true;
      return this;
    },
    write(chunk) {
      const bytes = asUint8Array(chunk);
      chunks.push(bytes);
      total += bytes.length;
      return true;
    },
    end(chunk) {
      if (chunk !== undefined) this.write(chunk);
      this.writableEnded = true;
      this.headersSent = true;
      resolve(new Response(concatChunks(chunks, total), { headers, status: this.statusCode }));
      return this;
    },
    flushHeaders() { this.headersSent = true; },
    cork() {},
    uncork() {},
    on() { return this; },
    once() { return this; },
    addListener() { return this; },
    removeListener() { return this; },
    emit() { return false; },
  };
}

async function dispatchNodeHandler(nodeHandlers, request) {
  const nodeRequest = await createNodeRequest(request);
  return await new Promise((resolve, reject) => {
    const nodeResponse = createNodeResponse(resolve);
    for (const nodeHandler of nodeHandlers) {
      try {
        const result = nodeHandler(nodeRequest, nodeResponse);
        if (result && typeof result.then === "function") result.catch(reject);
      } catch (err) {
        reject(err);
      }
    }
  });
}

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
  await installNodeHttpAdapter();
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
  // Express and other node:http servers: dispatch through the captured listener.
  if (!handler && capturedNodeHandlers.length > 0) {
    const handlers = [...capturedNodeHandlers];
    handler = (request) => dispatchNodeHandler(handlers, request);
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

// Streaming passthrough (story 16.D). Responses are written as TAGGED frames:
// `H` (header JSON {status, headers}), then zero or more `C` (raw body chunk),
// then `E` (end; JSON {error} if the stream failed mid-way). Finite bodies come
// through whole; infinite streams (SSE) flow chunk-by-chunk to the client. A
// single high byte cap remains as a runaway guard (clean truncation: the reader
// is cancelled and E is still written, so the connection stays framed-in-sync);
// stall protection lives on the Rust side (per-frame read timeout).
const STREAM_MAX_BYTES =
  parseInt(Deno.env.get("EDGER_STREAM_MAX_BYTES") ?? "", 10) || 256 * 1024 * 1024;
const MAX_CHUNK_FRAME = 1024 * 1024;

const FRAME_HEADER = "H".charCodeAt(0);
const FRAME_CHUNK = "C".charCodeAt(0);
const FRAME_END = "E".charCodeAt(0);

async function writeTagged(tag, payload) {
  const framed = new Uint8Array(1 + payload.length);
  framed[0] = tag;
  framed.set(payload, 1);
  await writeFrame(framed);
}

async function respond(handler, raw) {
  let response;
  try {
    response = await handler(buildRequest(raw));
  } catch (err) {
    const message = new TextEncoder().encode(String(err?.stack ?? err));
    await writeTagged(
      FRAME_HEADER,
      new TextEncoder().encode(
        JSON.stringify({ status: 500, headers: [["content-type", "text/plain"]] }),
      ),
    );
    await writeTagged(FRAME_CHUNK, message);
    await writeTagged(FRAME_END, new Uint8Array());
    return;
  }

  await writeTagged(
    FRAME_HEADER,
    new TextEncoder().encode(
      JSON.stringify({
        status: response.status,
        headers: Array.from(response.headers.entries()),
      }),
    ),
  );

  if (response.body) {
    const reader = response.body.getReader();
    let total = 0;
    let endPayload = new Uint8Array();
    try {
      while (total < STREAM_MAX_BYTES) {
        const { done, value } = await reader.read();
        if (done) break;
        const bytes = asUint8Array(value);
        total += bytes.length;
        for (let offset = 0; offset < bytes.length; offset += MAX_CHUNK_FRAME) {
          await writeTagged(FRAME_CHUNK, bytes.subarray(offset, offset + MAX_CHUNK_FRAME));
        }
      }
    } catch (err) {
      endPayload = new TextEncoder().encode(
        JSON.stringify({ error: String(err?.stack ?? err) }),
      );
    } finally {
      try {
        await reader.cancel();
      } catch (_) {
        // reader already released / stream errored — nothing to clean up.
      }
    }
    await writeTagged(FRAME_END, endPayload);
    return;
  }

  await writeTagged(FRAME_END, new Uint8Array());
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
    // A shutdown control frame is not a request: drain and exit cleanly.
    if (raw && raw.__control === "shutdown") {
      const drained = await handleShutdown(raw.reason ?? "shutdown", raw.graceMs ?? 0);
      try {
        await sendJson({ shutdown: "done", drained });
      } catch (_) {
        // socket already closed — nothing to ack.
      }
      break;
    }
    // respond() writes the tagged H/C.../E frames itself and never throws for
    // handler errors; a throw here means the socket broke — exit the loop.
    await respond(handler, raw);
  }
  // Explicit exit so the native `beforeunload` cannot double-fire the handlers
  // we already dispatched above.
  Deno.exit(0);
}

main().catch((err) => {
  console.error("harness fatal:", err);
  Deno.exit(1);
});
