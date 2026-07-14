type RouteRequest = Request & { params: Record<string, string> };

export default {
  routes: {
    "/api/status": () => Response.json({ ok: true }),
    "/users/:id": (req: RouteRequest) => Response.json({ user: req.params.id }),
    "/admin": {
      GET: () => new Response("admin-get"),
    },
    "/files/*": () => new Response("wildcard"),
  },
  fetch: () => new Response("fallback"),
};
