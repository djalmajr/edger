const { serve } = require("@hono/node-server");
const { Hono } = require("hono");
const app = new Hono();
const port = 8080;

// edger mounts each worker under /<name> and strips that prefix, so the worker
// sees "/" (mirrors hono-demo). A hardcoded "/commonjs-hono" would 404 here.
app.get("/", (c) => {
  return c.text("Hello, World!");
});

serve({
  fetch: app.fetch,
  port,
  overrideGlobalObjects: false,
});
