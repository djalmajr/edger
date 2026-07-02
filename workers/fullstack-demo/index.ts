// Fullstack/SSR requires an adapter; the runtime answers 501 adapter-required
// before this handler would run. Kept as the migration target placeholder.
Deno.serve(() => new Response("ssr placeholder"));
