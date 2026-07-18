import Fastify from "npm:fastify@5.6.1";
import { Readable } from "node:stream";

const app = Fastify({ logger: false });
let requests = 0;

app.addHook("onRequest", async (_request, reply) => {
  requests += 1;
  reply.header("x-fastify-hook", "active");
});

app.get("/", async () => ({ framework: "fastify", requests }));
app.get<{ Params: { id: string } }>("/users/:id", async (request) => ({
  user: request.params.id,
}));
app.post<{ Body: { message: string } }>(
  "/validate",
  {
    schema: {
      body: {
        type: "object",
        required: ["message"],
        properties: { message: { type: "string", minLength: 3 } },
      },
    },
  },
  async (request) => ({ message: request.body.message }),
);
app.get("/stream", async (_request, reply) =>
  reply.type("text/plain").send(Readable.from(["fastify-", "stream"])),
);

await app.listen({ port: 3000, host: "127.0.0.1" });
