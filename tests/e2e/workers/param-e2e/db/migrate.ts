// Release-hook migrator: o edger roda isto UMA vez por deploy (comando declarado no
// manifest), NÃO no cold-start path. Também serve como fallback app-owned no boot.
//
// Usa CONEXÃO DIRETA (bypass do pooler) porque advisory locks são de sessão e não
// sobrevivem a transaction pooling. Serializa runners concorrentes com
// pg_advisory_xact_lock e aplica migrations idempotentemente, rastreadas em _migrations.
import postgres from "postgres";

const DIRECT_URL = Deno.env.get("DIRECT_DATABASE_URL") ??
  "postgres://app:app@localhost:5432/app";
const MIGRATIONS_DIR = new URL("./schema/", import.meta.url);
const LOCK_KEY = 4242; // app-scoped: distinto por app/schema para não serializar apps diferentes

const sql = postgres(DIRECT_URL, { max: 1, prepare: false, onnotice: () => {} });

function migrationFiles(): string[] {
  return [...Deno.readDirSync(MIGRATIONS_DIR)]
    .filter((e) => e.isFile && e.name.endsWith(".sql"))
    .map((e) => e.name)
    .sort();
}

try {
  const applied = await sql.begin(async (tx) => {
    await tx`select pg_advisory_xact_lock(${LOCK_KEY})`;
    await tx`
      create table if not exists _migrations (
        name text primary key,
        applied_at timestamptz not null default now()
      )`;
    const done = new Set(
      (await tx`select name from _migrations`).map((r) => r.name as string),
    );

    const results: string[] = [];
    for (const file of migrationFiles()) {
      if (done.has(file)) continue;
      const ddl = await Deno.readTextFile(new URL(file, MIGRATIONS_DIR));
      await tx.unsafe(ddl).simple();
      await tx`insert into _migrations (name) values (${file})`;
      results.push(file);
    }
    return results;
  });

  console.log(
    applied.length === 0
      ? "up to date — no pending migrations"
      : `applied ${applied.length}: ${applied.join(", ")}`,
  );
} finally {
  await sql.end({ timeout: 5 });
}
