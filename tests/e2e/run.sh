#!/usr/bin/env bash
# End-to-end system test for edger Track B on a real running binary:
#   B1  worker receives DATABASE_URL and queries Postgres via PgBouncer
#   B2  beforeunload + EdgeRuntime.waitUntil drains the pool on TTL recycle
#   B3  the release phase runs migrations once before serving
#
# Brings up Postgres + PgBouncer (docker compose), boots ./target/debug/edger
# against the param-e2e worker, and asserts each capability. Reproducible:
#   tests/e2e/run.sh
# Requires docker + deno + cargo; SKIPS (exit 0) if docker/deno are unavailable.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PORT="${EDGER_E2E_PORT:-19080}"
BASE="http://127.0.0.1:${PORT}"
AUTH="authorization: Bearer test-root"
WORKER_DIR="$REPO_ROOT/tests/fixtures/param-e2e"
COMPOSE=(docker compose --project-directory "$SCRIPT_DIR" -f "$SCRIPT_DIR/docker-compose.yml")

command -v docker >/dev/null || { echo "SKIP: docker not found"; exit 0; }
command -v deno   >/dev/null || { echo "SKIP: deno not found"; exit 0; }
docker info >/dev/null 2>&1   || { echo "SKIP: docker daemon not running"; exit 0; }

EDGER_PID=""
cleanup() {
  [ -n "$EDGER_PID" ] && kill "$EDGER_PID" 2>/dev/null || true
  lsof -ti:"$PORT" 2>/dev/null | xargs kill -9 2>/dev/null || true
  "${COMPOSE[@]}" down -v >/dev/null 2>&1 || true
  rm -f "$WORKER_DIR/.edger-release" "$SCRIPT_DIR/edger.log"
}
trap cleanup EXIT

fail() { echo "FAIL: $1"; exit 1; }
psql_c() { "${COMPOSE[@]}" exec -T postgres psql "postgresql://app:app@localhost:5432/app" -tAc "$1" 2>/dev/null | tr -d '[:space:]'; }
count()  { local c; c="$(psql_c "$1")"; echo "${c:-0}"; }

echo "==> building edger binary"
( cd "$REPO_ROOT" && cargo build -q -p edger-orchestrator --bin edger ) || fail "cargo build"

echo "==> starting Postgres + PgBouncer (fresh volume)"
"${COMPOSE[@]}" up -d >/dev/null 2>&1 || fail "compose up"
for i in $(seq 1 30); do psql_c "select 1" | grep -q 1 && break; sleep 1; [ "$i" = 30 ] && fail "postgres never became ready"; done

echo "==> booting edger (release phase applies migrations before serving)"
rm -f "$WORKER_DIR/.edger-release"
lsof -ti:"$PORT" 2>/dev/null | xargs kill -9 2>/dev/null || true
( cd "$REPO_ROOT" && ROOT_API_KEY=test-root PORT="$PORT" \
    RUNTIME_WORKER_DIRS="$REPO_ROOT/tests/fixtures" RUST_LOG=warn ./target/debug/edger ) \
    >"$SCRIPT_DIR/edger.log" 2>&1 &
EDGER_PID=$!

# B1: the worker got DATABASE_URL and queried the DB via PgBouncer.
curl -s --retry 40 --retry-delay 1 --retry-connrefused -H "$AUTH" "$BASE/param-e2e/health" -o /tmp/edger-e2e-health.json || true
grep -q '"ok":1' /tmp/edger-e2e-health.json || fail "B1 /health (DB via pgbouncer): $(cat /tmp/edger-e2e-health.json 2>/dev/null)"
echo "PASS B1  /health -> $(cat /tmp/edger-e2e-health.json)"

# B3: the release phase ran migrations (marker table + _migrations + on-disk marker).
[ "$(count "select count(*) from e2e_release_marker")" -ge 1 ] || fail "B3 e2e_release_marker empty"
[ "$(count "select count(*) from _migrations")" -ge 2 ]       || fail "B3 _migrations < 2"
[ -f "$WORKER_DIR/.edger-release" ]                            || fail "B3 .edger-release marker missing"
echo "PASS B3  release applied migrations (marker + _migrations=$(count "select count(*) from _migrations") + .edger-release)"

# B1 (cont.): parameterized query via PgBouncer returns the seeded tree.
PARAMS="$(curl -s -H "$AUTH" "$BASE/param-e2e/?tenant=11111111-1111-1111-1111-111111111111")"
echo "$PARAMS" | grep -q '"count":3'      || fail "B1 /params count (via pgbouncer): $PARAMS"
echo "$PARAMS" | grep -q '"featureFlags"' || fail "B1 /params content: $PARAMS"
echo "PASS B1  /params -> count:3 (parameterized query via pgbouncer)"

# B2: graceful SIGTERM drains the live worker. main() awaits the pool drain before
# exiting, so beforeunload + EdgeRuntime.waitUntil run the same cleanup as a recycle.
# The worker is warm + connected from the B1 curls; its long TTL keeps it live.
echo "==> sending SIGTERM (graceful shutdown must drain the live worker before exit)"
kill -TERM "$EDGER_PID" 2>/dev/null || true
for i in $(seq 1 20); do kill -0 "$EDGER_PID" 2>/dev/null && sleep 1 || break; done
EDGER_PID="" # already exited; don't double-kill in cleanup
[ "$(count "select count(*) from e2e_shutdown_log where reason='terminate'")" -ge 1 ] \
  || fail "B2 SIGTERM did not drain the worker (no e2e_shutdown_log reason=terminate)"
echo "PASS B2  SIGTERM drained the pool before exit (e2e_shutdown_log reason=terminate)"

echo
echo "E2E PASSED — B1 (env + pgbouncer) · B2 (graceful SIGTERM drain) · B3 (release migrations)"
