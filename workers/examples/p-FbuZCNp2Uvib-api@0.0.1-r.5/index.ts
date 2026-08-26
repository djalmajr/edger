type Handler = (req: Request) => Response | Promise<Response>;
type ApiModule = Record<string, unknown>;
type ApiFactory = (require: (path: string) => ApiModule) => ApiModule;

const factories: Record<string, ApiFactory> = {
  "~api/consultas": (require) => {
    function json(payload: unknown, status = 200) {
      return Response.json(payload, { status });
    }
    
     const GET = () =>
      json({ consultas: [{ id: 1, paciente: "Ana" }], ok: true });
    
     async function POST(req: Request) {
      const body = await req.json();
      return json({ criado: true, ...body }, 201);
    }
    
    const __studioDefaultExport = (req: Request) =>
      json({ fallback: true, method: req.method });
    return { "GET": GET, "POST": POST, "default": __studioDefaultExport };
  },
};

const cache = new Map<string, ApiModule>();
function requireApi(path: string): ApiModule {
  const cached = cache.get(path);
  if (cached) return cached;
  const factory = factories[path];
  if (!factory) throw new Error(`Unknown API module: ${path}`);
  const value = factory(requireApi);
  cache.set(path, value);
  return value;
}

function dispatch(path: string, allowedMethods: readonly string[]): Handler {
  return async (request) => {
    const apiModule = requireApi(path);
    const method = request.method.toUpperCase();
    const candidate = apiModule[method] ?? apiModule.default;
    if (typeof candidate === "function") return candidate(request);
    return new Response("method not allowed", {
      headers: { Allow: allowedMethods.join(", ") },
      status: 405,
    });
  };
}

export const routes: Record<string, Handler> = {
  "/consultas": dispatch("~api/consultas", ["GET","POST"]),
};
