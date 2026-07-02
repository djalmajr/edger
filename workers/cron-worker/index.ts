Deno.serve((req: Request) => {
  const payload = {
    ok: true,
    path: new URL(req.url).pathname,
    method: req.method,
    internal: req.headers.get("x-edger-internal") === "true",
  };

  return new Response(JSON.stringify(payload), {
    headers: { "content-type": "application/json" },
  });
});
