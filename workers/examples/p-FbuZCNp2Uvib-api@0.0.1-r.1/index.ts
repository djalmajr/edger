type Handler = (req: Request) => Response | Promise<Response>;
type ApiModule = Record<string, unknown>;
type ApiFactory = (require: (path: string) => ApiModule) => ApiModule;

const factories: Record<string, ApiFactory> = {
  "~api": (require) => {
    return {  };
  },
  "~api/consultas": (require) => {
    const GET: Handler = ((req) => Response.json({ consultas: [{ id: 1, paciente: "Ana" }], ok: true }));
    const POST: Handler = (async (req) => { const body = await req.json(); return Response.json({ criado: true, ...body }, { status: 201 }); });
    const validar = ((dado) => Boolean(dado));
    return { GET, POST, validar };
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

function handler(path: string, method: string): Handler {
  const candidate = requireApi(path)[method];
  if (typeof candidate !== "function") {
    throw new Error(`API handler not found: ${method} ${path}`);
  }
  return candidate as Handler;
}

export const routes = {
  "/consultas": { GET: handler("~api/consultas", "GET"), POST: handler("~api/consultas", "POST") },
};
