import Koa from "npm:koa@3.0.1";
import Router from "npm:@koa/router@14.0.0";
import { bodyParser } from "npm:@koa/bodyparser@6.0.0";
import { Readable } from "node:stream";

const app = new Koa({ asyncLocalStorage: true });
const router = new Router();
let requests = 0;

app.use(async (context, next) => {
  requests += 1;
  context.set("x-koa-middleware", "active");
  await next();
  context.set("x-koa-upstream", "resumed");
});
app.use(bodyParser());

router.get("/", (context) => {
  context.body = { framework: "koa", requests };
});
router.get("/users/:id", (context) => {
  context.body = { user: context.params.id };
});
router.post("/validate", (context) => {
  const body = context.request.body as { message?: unknown };
  if (typeof body.message !== "string" || body.message.length < 3) {
    context.throw(422, "message must have at least 3 characters");
  }
  context.body = { message: body.message };
});
router.get("/stream", (context) => {
  context.type = "text/plain";
  context.body = Readable.from(["koa-", "stream"]);
});

app.use(router.routes());
app.use(router.allowedMethods());
app.listen(3000);
