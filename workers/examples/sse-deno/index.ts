// workers/examples/sse-deno/index.ts
//
// Deno guest for the host-held SSE of EdgeR (EDG-55). It authorizes and
// publishes in SHORT executions; the host keeps every browser connection.
//
//   GET  /events  Bearer token vs the vault secret SSE_ACCESS_TOKEN: 200 with
//                 `x-edger-sse-subscribe: room.1`, or 401
//   POST /publish same authorization; the JSON body
//                 {"events":[{"topic","data","event","id"}]} (the test's
//                 control channel) becomes 1..8 `x-edger-sse-publish`
//                 directives; an empty list publishes nothing
//   GET  /plain   an ordinary answer, no SSE
//   GET  /stream  an ordinary ReadableStream: ?n= chunks spaced ?gap= ms
//   GET  /cookies an ordinary answer with two Set-Cookie headers
//
// Test-only controls: each `topics=` query parameter of /events becomes one
// `x-edger-sse-subscribe` header (instead of the default topic) and the `raw`
// list of the /publish body is appended verbatim as extra
// `x-edger-sse-publish` values.

const SUBSCRIBE_TOPIC = "room.1";

interface ControlEvent {
  data: string;
  event?: string;
  id?: string;
  topic: string;
}

interface ControlBody {
  events?: ControlEvent[];
  raw?: string[];
}

function base64Url(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function json(status: number, body: unknown, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    headers: { "content-type": "application/json", ...headers },
    status,
  });
}

function authorized(request: Request): boolean {
  const token = Deno.env.get("SSE_ACCESS_TOKEN") ?? "";
  if (token === "") {
    return false;
  }
  return (request.headers.get("authorization") ?? "") === `Bearer ${token}`;
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function publish(request: Request): Promise<Response> {
  if (!authorized(request)) {
    return json(401, { error: "unauthorized" });
  }
  const raw = await request.text();
  let control: ControlBody = {};
  if (raw.length > 0) {
    try {
      control = JSON.parse(raw) as ControlBody;
    } catch {
      return json(400, { error: "bad request" });
    }
  }
  const events = control.events ?? [];
  const headers = new Headers({ "content-type": "application/json" });
  for (const event of events) {
    headers.append("x-edger-sse-publish", base64Url(JSON.stringify(event)));
  }
  const rawValues = control.raw ?? [];
  for (const value of rawValues) {
    headers.append("x-edger-sse-publish", value);
  }
  return new Response(JSON.stringify({ published: events.length + rawValues.length }), {
    headers,
    status: 200,
  });
}

function stream(url: URL): Response {
  const count = Number(url.searchParams.get("n") ?? "5");
  const gap = Number(url.searchParams.get("gap") ?? "200");
  const encoder = new TextEncoder();
  let sent = 0;
  const body = new ReadableStream<Uint8Array>({
    async pull(controller) {
      if (sent >= count) {
        controller.close();
        return;
      }
      if (sent > 0) {
        await sleep(gap);
      }
      controller.enqueue(encoder.encode(`chunk-${sent}\n`));
      sent += 1;
    },
  });
  return new Response(body, { headers: { "content-type": "text/plain" }, status: 200 });
}

export default {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/events" && request.method === "GET") {
      if (!authorized(request)) {
        return json(401, { error: "unauthorized" });
      }
      const topics = url.searchParams.getAll("topics");
      const headers = new Headers({ "content-type": "application/json" });
      for (const topic of topics.length > 0 ? topics : [SUBSCRIBE_TOPIC]) {
        headers.append("x-edger-sse-subscribe", topic);
      }
      return new Response(JSON.stringify({ subscribed: true }), { headers, status: 200 });
    }
    if (url.pathname === "/publish") {
      return await publish(request);
    }
    if (url.pathname === "/plain") {
      return json(200, { plain: true });
    }
    if (url.pathname === "/cookies") {
      const headers = new Headers({ "content-type": "application/json" });
      headers.append("set-cookie", "a=1; Path=/");
      headers.append("set-cookie", "b=2; Path=/");
      return new Response(JSON.stringify({ cookies: 2 }), { headers, status: 200 });
    }
    if (url.pathname === "/stream") {
      return stream(url);
    }
    return json(404, { error: "not found" });
  },
};
