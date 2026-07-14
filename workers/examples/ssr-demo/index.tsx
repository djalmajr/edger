// Fullstack blessed path (story 16.A): Hono SSR + JSX, deployed as SOURCE.
// Deno transpiles the .tsx natively (deno.json: jsxImportSource hono/jsx) —
// no build step. SSR page + JSON API in the same worker.
import { Hono } from "hono";
import { jsxRenderer } from "hono/jsx-renderer";

const app = new Hono();
const startedAt = new Date();

app.use(
  "*",
  jsxRenderer(({ children }) => (
    <html lang="pt-br">
      <head>
        <meta charset="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <link rel="icon" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'><text y='13' font-size='13'>⚡</text></svg>" />
        <title>EdgeR SSR demo</title>
        <style>{`
          body { font-family: system-ui, sans-serif; margin: 3rem auto; max-width: 40rem; color: #151923; }
          code { background: #eef2f7; border-radius: 4px; padding: 2px 6px; }
          .card { border: 1px solid #dfe3eb; border-radius: 8px; padding: 1rem 1.5rem; margin-top: 1rem; }
        `}</style>
      </head>
      <body>{children}</body>
    </html>
  )),
);

app.get("/", (c) => {
  const renderedAt = new Date().toISOString();
  // EdgeR mounts the worker under a base path and strips it from the request;
  // links must be absolute WITH the base (relative links break on the bare
  // `/name` URL, which has no trailing slash).
  const base = c.req.header("x-base") ?? "";
  return c.render(
    <main>
      <h1>EdgeR — SSR com Hono + JSX</h1>
      <p>
        Esta página foi <strong>renderizada no servidor</strong> pelo processo
        Deno persistente, a partir de um <code>index.tsx</code> deployado como
        fonte (sem build).
      </p>
      <div class="card">
        <p>
          Renderizada em: <code data-testid="rendered-at">{renderedAt}</code>
        </p>
        <p>
          Worker no ar desde: <code>{startedAt.toISOString()}</code>
        </p>
        <p>
          API do mesmo worker: <a href={`${base}/api/info`}>api/info</a>
        </p>
      </div>
    </main>,
  );
});

app.get("/api/info", (c) =>
  c.json({
    worker: "ssr-demo",
    runtime: "deno-persistent-process",
    ssr: "hono/jsx",
    now: new Date().toISOString(),
  }),
);

Deno.serve(app.fetch);
