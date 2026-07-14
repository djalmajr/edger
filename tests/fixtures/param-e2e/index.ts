// e2e worker exercising the full Track B cycle on a running edger:
//   B1 (DATABASE_URL delivered) + query via pgbouncer, and
//   B2 (beforeunload drains the pool on graceful recycle — observable via a DB row).
import postgres from "postgres";

const sql = postgres(Deno.env.get("DATABASE_URL") ?? "", {
  max: 4,
  prepare: true,
  idle_timeout: 20,
  fetch_types: false,
});

addEventListener("beforeunload", (ev: Event) => {
  const reason = (ev as CustomEvent<{ reason?: string }>).detail?.reason ?? "?";
  console.error(`[param-e2e] beforeunload reason=${reason} — draining pool`);
  EdgeRuntime.waitUntil((async () => {
    try {
      await sql`insert into e2e_shutdown_log (reason) values (${reason})`;
    } finally {
      await sql.end({ timeout: 3 });
    }
  })());
});

export default {
  async fetch(req: Request): Promise<Response> {
    const url = new URL(req.url);
    if (url.pathname.endsWith("/health")) {
      const [{ ok }] = await sql`select 1 as ok`;
      return Response.json({ ok, via: "pgbouncer" });
    }
    const tenant = url.searchParams.get("tenant") ?? "11111111-1111-1111-1111-111111111111";
    const rows = await sql`
      select key, value, value_type, parent_id
      from cluster_space_parameter
      where tenant_id = ${tenant}
      order by parent_id nulls first, key`;
    return Response.json({ tenant, count: rows.length, rows });
  },
};
