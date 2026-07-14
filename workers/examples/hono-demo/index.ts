import { Hono } from "npm:hono@4";

const app = new Hono();
app.get("/", (c) => c.json({ framework: "hono" }));
app.get("/users/:id", (c) => c.json({ user: c.req.param("id") }));

Deno.serve(app.fetch);
