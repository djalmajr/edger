import { describe, it, expect } from "bun:test";
import { loadWorkerHandler } from "./edger";

describe("edger loadWorkerHandler (Deno.serve + default export compat)", () => {
  it("loads hello-world and returns correct json for POST", async () => {
    const handler = await loadWorkerHandler("./workers/hello-world");
    const req = new Request("http://x/", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: "Alice" }),
    });
    const res = await handler(req);
    expect(res.status).toBe(200);
    const json = await res.json();
    expect(json.message).toContain("Hello Alice from foo");
  });

  it("loads serve-declarative-style and returns Hello, world", async () => {
    const handler = await loadWorkerHandler("./workers/serve-declarative-style");
    const req = new Request("http://x/");
    const res = await handler(req);
    expect(res.status).toBe(200);
    const text = await res.text();
    expect(text).toBe("Hello, world");
  });

  it("loads empty-response and returns 204", async () => {
    const handler = await loadWorkerHandler("./workers/empty-response");
    const req = new Request("http://x/");
    const res = await handler(req);
    expect(res.status).toBe(204);
  });

  it("loads read-body and returns totalSize json", async () => {
    const handler = await loadWorkerHandler("./workers/read-body");
    const req = new Request("http://x/", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "12345",
    });
    const res = await handler(req);
    expect(res.status).toBe(200);
    const json = await res.json();
    expect(json.totalSize).toBe(5);
  });

  it("loads chunked-text and returns meow stream text", async () => {
    const handler = await loadWorkerHandler("./workers/chunked-text");
    const req = new Request("http://x/");
    const res = await handler(req);
    expect(res.status).toBe(200);
    const text = await res.text();
    expect(text).toBe("meow");
  });

  it("loads serve-html and serves foo.html content via Deno.readTextFile shim", async () => {
    const handler = await loadWorkerHandler("./workers/serve-html");
    const req = new Request("http://x/foo");
    const res = await handler(req);
    expect(res.status).toBe(200);
    const text = await res.text();
    expect(text).toContain("Foo");
  });
});
