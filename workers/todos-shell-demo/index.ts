Deno.serve((req: Request) => {
  const url = new URL(req.url);
  return new Response(
    JSON.stringify({
      base: req.headers.get("x-base"),
      path: url.pathname,
    }),
    {
      headers: { "content-type": "application/json" },
    },
  );
});
