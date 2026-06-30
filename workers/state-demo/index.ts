Deno.serve((req: Request) => {
  const bindings = req.headers.get("x-edger-bindings");
  return new Response(
    JSON.stringify({
      path: new URL(req.url).pathname,
      bindings: bindings ? JSON.parse(bindings) : null,
    }),
    {
      headers: { "content-type": "application/json" },
    },
  );
});
