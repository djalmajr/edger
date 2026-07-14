import { message } from "./message.ts";

Deno.serve(() => new Response(message()));
