Deno.serve((request) => {
  const url = new URL(request.url);
  if (url.pathname.endsWith("/health")) {
    return Response.json({ status: "ok" });
  }
  return Response.json({ worker: "health-demo", version: "1.0.0" });
});
