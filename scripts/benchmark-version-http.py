#!/usr/bin/env python3
"""End-to-end HTTP benchmark for version routing + pool behavior (EXECUTED
2026-10-01 under the orchestrator's execution amendment; re-run with the
commands below).

Reproducible run (release build first; loopback only, own ports/PIDs, own
process-group cleanup; no preview/labdev ports):
  CARGO_TARGET_DIR=/tmp/edger-version-release-target-20261001 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
    cargo +1.98.0 build --release -p edger-orchestrator --bin edger \
    --example version-routing-benchmark
  python3 scripts/benchmark-version-http.py \
    --edger-bin /tmp/edger-version-release-target-20261001/release/edger \
    --deno-bin /opt/homebrew/bin/deno \
    --out /tmp/edger-version-bench-results-20261001/http
  (defaults: V list 1,10,100; churn 40; 3 reps; 20 warmup + 60 measured hits
   per rep; --help lists the real flags; exit code is non-zero when any
   scenario or request fails)

What it measures (real orchestrator + real Deno persistent processes — this
is NOT the microbenchmark and must not be conflated with it):

* hot-single (V in 1, 10, 100): one fresh server per V with exactly V
  versions of the SAME app installed; repeated hits on version 1.0.0:
    - pinned route  GET /bench-app@1.0.0   (version pinned)
    - explicit default: POST /api/admin/workers/bench-app/promote?version=1.0.0
      then repeated GET /bench-app          (unversioned route -> promoted)
  Each phase runs SEQUENTIAL REPS (default 3) on the SAME server/process —
  not independent experiments: the same pool and Deno processes persist
  across reps. Within each rep the first
  WARMUP_PER_REP hits (default 20) are EXCLUDED from the statistics; only the
  SAMPLES_PER_REP hits (default 60) are measured. The single first cold hit
  (process spawn) is reported as a POINT (n=1), never as a population
  percentile.
* churn (V = 40 by default, must be > 32 = pool LRU capacity): one server
  with 40 versions; sweep all 40 once (each first hit is a cold point; by
  the end the LRU holds only the last 32, so 1.0.0..1.0.7 were evicted);
  then:
    - readmission:  GET /bench-app@1.0.0   (evicted -> must spawn again)
    - warm control: GET /bench-app@1.0.39  (still cached)
    - post-readmission warm: GET /bench-app@1.0.0 again
  RECREATION IS PROVEN BY PROCESS GENERATION, NOT BY LATENCY: every worker
  module creates `const instance = crypto.randomUUID()` at import time and
  echoes {app, version, instance}. Readmission must return a DIFFERENT
  instance than the original sweep hit of 1.0.0; the post-readmission hit
  must return the SAME instance as the readmission; the warm control must
  match its own sweep instance. `readmission_proven` is that instance-based
  check (latencies are still reported, but only as data).

Methodology:
* loopback only (127.0.0.1) on a freshly allocated free port per server run;
  the preview ports 19080/19081 (and anything labdev-related) are refused.
* ephemeral fixture under /tmp (mkdtemp prefix edger-version-bench-): worker
  versions with an identifiable response body {"app","version","instance"},
  empty core / core-overlay dirs, api-keys db inside the fixture. The root
  key is generated at runtime (secrets.token_hex) and NEVER written to the
  results or logs (no key prefixes, no env dumps).
* The server environment EXPLICITLY disables the routing opt-ins and OTEL
  so the benchmark does not depend on the operator's environment:
  EDGER_TENANT_ROUTING_ENABLED=0, EDGER_WEIGHTED_ROUTING_ENABLED=0,
  EDGER_OTEL_ENABLED=0, OTEL_EXPORTER_OTLP_ENDPOINT="" (values verified
  against opt_in_flag/env_flag semantics: "0"/empty -> off), and forces
  EDGER_JS_RUNTIME=process (only "bridge" switches the runtime —
  bin/edger.rs — so the persistent-process fallback is never inherited).
* per-request verification of the FULL body contract: status 200, app name,
  version, non-empty process instance, and generation stability — every
  hot warmup/warm hit must keep the instance of the first cold hit; a
  divergent body or generation is a failure, never reported as warm.
  Errors, mismatches and timeouts are counted separately. Retries are
  VISIBLE:
  latency is measured from before the FIRST attempt (a retried request
  carries the full wall time) and the retry count per request is recorded;
  aggregates report the total retries, so "0 errors" is never reported when
  connections were re-established.
* latency = full request round-trip (send to body complete), ms
  (p50/p95/p99 + mean/min/max for n>=2; n=1 reported as a point).
* cleanup: ONLY this script's own processes — the server runs in its own
  process group (start_new_session); on stop the group is SIGTERM'd, waited
  on, then SIGKILL'd; a pgrep -f sweep against the UNIQUE fixture path picks
  up any surviving Deno children (their command lines carry the fixture
  worker dir) and only those. Fixtures are removed even on failure;
  `--keep` preserves them.

Constraints honored: stdlib only (no new dependencies), no production code
or config changes, no repo writes (fixture + results live under /tmp), the
repo target/release binary is used in place (never copied), and building
only happens with the explicit --build flag at run time.

Usage:
  python3 scripts/benchmark-version-http.py --build
  python3 scripts/benchmark-version-http.py --edger-bin /path/to/edger
  (options in --help; defaults: V list 1,10,100; churn 40; reps 3;
   20 warmup + 60 measured hits per rep)
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import http.client
import json
import math
import os
import platform
import shutil
import secrets
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

APP_NAME = "bench-app"
HOT_VERSION = "1.0.0"
RESERVED_PORTS = {19080, 19081}  # preview/labdev ports — never used
POOL_LRU_CAPACITY = 32  # edger-worker PoolConfig::default max_size (types.rs)
READINESS_TIMEOUT_S = 120.0
REQUEST_TIMEOUT_S = 30.0
LOG_TAIL_LINES = 40
RECORD_FAILURE_CAP = 50


def version_str(i: int) -> str:
    return f"1.0.{i}"


def log(msg: str) -> None:
    print(f"[vbench] {msg}", flush=True)


def find_free_loopback_port() -> int:
    for _ in range(32):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        if port not in RESERVED_PORTS:
            return port
    raise RuntimeError(f"could not allocate a free loopback port outside {sorted(RESERVED_PORTS)}")


def write_fixture(fixture: Path, versions: int) -> None:
    (fixture / "workers").mkdir(parents=True)
    (fixture / "core").mkdir(parents=True)  # empty core root (required to exist)
    (fixture / "core-overlay").mkdir(parents=True)
    for i in range(versions):
        version = version_str(i)
        worker_dir = fixture / "workers" / f"{APP_NAME}-{i:03d}"
        worker_dir.mkdir(parents=True)
        (worker_dir / "manifest.yaml").write_text(
            f"name: {APP_NAME}\n"
            f'version: "{version}"\n'
            "kind: fetch\n"
            "entrypoint: index.ts\n",
            encoding="utf-8",
        )
        (worker_dir / "index.ts").write_text(
            "const APP = "
            f'"{APP_NAME}";\n'
            "const VERSION = "
            f'"{version}";\n'
            "const instance = crypto.randomUUID();\n"
            "Deno.serve((req: Request) => {\n"
            "  return new Response(JSON.stringify({ app: APP, version: VERSION, instance }), {\n"
            '    headers: { "content-type": "application/json" },\n'
            "  });\n"
            "});\n",
            encoding="utf-8",
        )


class ServerHandle:
    """Owns the benchmark orchestrator process (its own process group) and
    its cleanup. Cleanup touches ONLY this server's group and any Deno
    children that reference the unique fixture path."""

    def __init__(self, binary: Path, env: dict, cwd: Path, log_path: Path, fixture: Path) -> None:
        self.pgid: int | None = None
        self.log_file = open(log_path, "ab")
        self.proc = subprocess.Popen(
            [str(binary)],
            env=env,
            cwd=str(cwd),
            stdout=self.log_file,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            self.pgid = os.getpgid(self.proc.pid)
        except OSError:
            self.pgid = None
        self.pid = self.proc.pid
        self.port = int(env["PORT"])
        self.root_key = env["ROOT_API_KEY"]
        self.fixture = fixture

    def _sweep_fixture_children(self) -> None:
        """SIGTERM/SIGKILL processes whose command line references OUR unique
        fixture path (the Deno children carry the absolute worker dir)."""
        try:
            out = subprocess.run(
                ["pgrep", "-f", str(self.fixture)],
                capture_output=True,
                text=True,
                timeout=10,
            )
            pids = [int(x) for x in out.stdout.split()]
        except Exception:  # noqa: BLE001 - sweep is best-effort
            return
        me = os.getpid()
        for pid in pids:
            if pid == me:
                continue
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        if pids:
            time.sleep(0.5)
        for pid in pids:
            if pid == me:
                continue
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    def stop(self) -> None:
        proc = self.proc
        if proc.poll() is None:
            if self.pgid is not None:
                try:
                    os.killpg(self.pgid, signal.SIGTERM)
                except OSError:
                    pass
            else:
                proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                if self.pgid is not None:
                    try:
                        os.killpg(self.pgid, signal.SIGKILL)
                    except OSError:
                        pass
                proc.kill()
                try:
                    proc.wait(timeout=10)
                except Exception:  # noqa: BLE001
                    pass
        self._sweep_fixture_children()
        try:
            self.log_file.close()
        except Exception:  # noqa: BLE001
            pass


def wait_ready(base: str, timeout_s: float) -> None:
    deadline = time.monotonic() + timeout_s
    last_err = ""
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"{base}/ready", timeout=3) as resp:
                if resp.status == 200:
                    return
                last_err = f"status {resp.status}"
        except Exception as err:  # noqa: BLE001 - readiness polling
            last_err = str(err)
        time.sleep(0.2)
    raise RuntimeError(f"server not ready under {timeout_s}s (last: {last_err})")


def log_tail(path: Path, lines: int = LOG_TAIL_LINES) -> str:
    try:
        content = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return ""
    return "\n".join(content[-lines:])


def build_env(fixture: Path, port: int, root_key: str, deno_bin: str) -> dict:
    env = os.environ.copy()
    env.update(
        {
            "ROOT_API_KEY": root_key,
            "EDGER_BIND": "127.0.0.1",
            "PORT": str(port),
            "RUNTIME_WORKER_DIRS": str(fixture / "workers"),
            "EDGER_CORE_WORKER_DIR": str(fixture / "core"),
            "EDGER_CORE_WORKER_OVERLAY_DIR": str(fixture / "core-overlay"),
            "EDGER_API_KEYS_DB": str(fixture / "api-keys.db"),
            "RUST_LOG": "warn",
            "EDGER_DENO_BIN": deno_bin,
            # Only "bridge" switches the JS runtime (bin/edger.rs:58-60);
            # force the persistent process runtime explicitly so the
            # benchmark never inherits an operator value.
            "EDGER_JS_RUNTIME": "process",
            # Explicit opt-outs: do not depend on the operator's environment.
            # ("0"/"" are the off values per opt_in_flag/env_flag semantics.)
            "EDGER_TENANT_ROUTING_ENABLED": "0",
            "EDGER_WEIGHTED_ROUTING_ENABLED": "0",
            "EDGER_OTEL_ENABLED": "0",
            "OTEL_EXPORTER_OTLP_ENDPOINT": "",
        }
    )
    return env


class BenchClient:
    """Tiny keep-alive client. Latency is measured from BEFORE the first
    attempt; retries are counted and returned, never hidden."""

    def __init__(self, host: str, port: int) -> None:
        self.host = host
        self.port = port
        self.conn: http.client.HTTPConnection | None = None

    def _connect(self) -> None:
        self.close()
        self.conn = http.client.HTTPConnection(self.host, self.port, timeout=REQUEST_TIMEOUT_S)

    def close(self) -> None:
        if self.conn is not None:
            try:
                self.conn.close()
            except Exception:  # noqa: BLE001
                pass
            self.conn = None

    def _request(self, method: str, path: str, headers: dict | None):
        if self.conn is None:
            self._connect()
        assert self.conn is not None
        retries = 0
        outer_start = time.perf_counter()
        for attempt in (1, 2):
            try:
                if headers is None:
                    self.conn.request(method, path)
                else:
                    self.conn.request(method, path, headers=headers)
                resp = self.conn.getresponse()
                body = resp.read()
                latency = time.perf_counter() - outer_start
                return resp.status, body, latency, retries, ""
            except (http.client.HTTPException, OSError) as err:
                retries += 1
                if attempt == 1:
                    self._connect()
                    continue
                return 0, b"", time.perf_counter() - outer_start, retries, str(err)
        return 0, b"", time.perf_counter() - outer_start, retries, "unreachable"

    def get(self, path: str):
        return self._request("GET", path, None)

    def post(self, path: str, api_key: str):
        return self._request("POST", path, {"x-api-key": api_key})


def percentile(values: list, p: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    rank = int(math.ceil((p / 100.0) * len(ordered)))  # nearest-rank
    rank = min(len(ordered), max(1, rank))
    return ordered[rank - 1]


def summarize(latencies_ms: list) -> dict:
    """n >= 2: percentiles of the population; n == 1: a POINT, not a
    population percentile (labeled as such)."""
    if not latencies_ms:
        return {"n": 0}
    if len(latencies_ms) == 1:
        return {"n": 1, "is_point": True, "point_ms": round(latencies_ms[0], 3)}
    return {
        "n": len(latencies_ms),
        "is_point": False,
        "p50_ms": round(percentile(latencies_ms, 50), 3),
        "p95_ms": round(percentile(latencies_ms, 95), 3),
        "p99_ms": round(percentile(latencies_ms, 99), 3),
        "mean_ms": round(sum(latencies_ms) / len(latencies_ms), 3),
        "min_ms": round(min(latencies_ms), 3),
        "max_ms": round(max(latencies_ms), 3),
    }


def hit(
    client: BenchClient,
    records: list,
    record_failures: list,
    scenario: str,
    v: int,
    phase: str,
    path: str,
    expected_version: str,
    cls: str,
    rep: int,
    hit_no: int,
    instance_track: dict | None = None,
    expected_instance: str | None = None,
):
    """One measured request. Consumes the full body and verifies the full
    contract: status 200, app name, version, non-empty process instance, and
    (when expected_instance is given) generation stability — a warm hit must
    keep the instance of the first cold hit; divergence is a failure and the
    hit is never reported as warm. Any divergence is a record-level failure
    (final exit code goes non-zero)."""
    status, body, latency_s, retries, error = client.get(path)
    parsed = {}
    if status == 200 and body:
        try:
            loaded = json.loads(body)
        except (ValueError, UnicodeDecodeError):
            loaded = None
        # A body that is valid JSON but not an object (e.g. [] or null) is a
        # contract divergence: force {} so the field checks below fail the
        # record instead of raising AttributeError.
        parsed = loaded if isinstance(loaded, dict) else {}
    ok = status == 200
    mismatch = ok and str(parsed.get("version", "")) != expected_version
    app_bad = ok and str(parsed.get("app", "")) != APP_NAME
    instance = str(parsed.get("instance", "")) if ok else ""
    empty_instance = ok and not instance
    divergent = (
        ok and bool(expected_instance) and bool(instance) and instance != expected_instance
    )
    record = {
        "scenario": scenario,
        "v": v,
        "phase": phase,
        "class": cls,
        "rep": rep,
        "hit": hit_no,
        "path": path,
        "status": status,
        "ok": ok,
        "mismatch": mismatch,
        "app_bad": app_bad,
        "empty_instance": empty_instance,
        "divergent_generation": divergent,
        "expected_instance_prefix": expected_instance[:8] if expected_instance else "",
        "retries": retries,
        "latency_ms": round(latency_s * 1000.0, 3),
        "error": error,
        "instance": instance,
    }
    records.append(record)
    if not ok:
        record_failures.append(f"{scenario} V={v} {phase} rep={rep} hit={hit_no}: status={status} error={error or 'non-200'}")
    elif mismatch:
        record_failures.append(
            f"{scenario} V={v} {phase} rep={rep} hit={hit_no}: version mismatch expected={expected_version} got={parsed.get('version')!r}"
        )
    elif app_bad:
        record_failures.append(
            f"{scenario} V={v} {phase} rep={rep} hit={hit_no}: app mismatch expected={APP_NAME!r} got={parsed.get('app')!r}"
        )
    elif empty_instance:
        record_failures.append(
            f"{scenario} V={v} {phase} rep={rep} hit={hit_no}: empty instance in body"
        )
    elif divergent:
        record_failures.append(
            f"{scenario} V={v} {phase} rep={rep} hit={hit_no}: generation divergence expected={expected_instance[:8]!r} got={instance[:8]!r} (not warm)"
        )
    if instance_track is not None and instance:
        instance_track[expected_version] = instance
    return record


def run_hot_single(cfg: dict) -> dict:
    """One scenario: fresh fixture + server with exactly V versions installed.
    Independent reps; warmup hits excluded from statistics; the single cold
    hit is reported as a point."""
    v: int = cfg["v"]
    reps: int = cfg["reps"]
    warmup: int = cfg["warmup_per_rep"]
    samples: int = cfg["samples_per_rep"]
    records: list = []
    record_failures: list = []
    server: ServerHandle | None = None
    fixture = Path(tempfile.mkdtemp(prefix="edger-version-bench-", dir="/tmp"))
    try:
        write_fixture(fixture, v)
        port = find_free_loopback_port()
        root_key = secrets.token_hex(16)
        log_path = fixture / "edger.log"
        log_path.touch()
        env = build_env(fixture, port, root_key, cfg["deno_bin"])
        # The handle is owned by the finally from the moment of spawn, so a
        # readiness failure cannot leak the process.
        server = ServerHandle(Path(cfg["edger_bin"]), env, fixture, log_path, fixture)
        base = f"http://127.0.0.1:{port}"
        wait_ready(base, READINESS_TIMEOUT_S)
        client = BenchClient("127.0.0.1", port)

        hot = HOT_VERSION
        # Phase A: pinned hot version. First hit = cold point; its instance
        # becomes the generation reference for every warmup/warm hit below
        # (a divergence is a failure, never reported as warm).
        cold = hit(client, records, record_failures, "hot-single", v, "hot-pinned",
            f"/{APP_NAME}@{hot}", hot, "cold", rep=0, hit_no=0)
        hot_instance = cold["instance"]
        for rep in range(reps):
            for k in range(warmup):
                hit(client, records, record_failures, "hot-single", v, "hot-pinned",
                    f"/{APP_NAME}@{hot}", hot, "warmup", rep=rep, hit_no=k,
                    expected_instance=hot_instance)
            for k in range(samples):
                hit(client, records, record_failures, "hot-single", v, "hot-pinned",
                    f"/{APP_NAME}@{hot}", hot, "warm", rep=rep, hit_no=k,
                    expected_instance=hot_instance)

        # Phase B: explicit default via promote, then the unversioned route.
        # The 1.0.0 process is already warm from phase A, so every hit here
        # is warm at the process level (the unversioned route cost is what
        # this phase isolates).
        status, _body, _latency, retries, error = client.post(
            f"/api/admin/workers/{APP_NAME}/promote?version={hot}", root_key
        )
        if status != 200:
            record_failures.append(
                f"hot-single V={v} promote: status={status} error={error}"
            )
        for rep in range(reps):
            for k in range(warmup):
                hit(client, records, record_failures, "hot-single", v, "hot-default",
                    f"/{APP_NAME}", hot, "warmup", rep=rep, hit_no=k,
                    expected_instance=hot_instance)
            for k in range(samples):
                hit(client, records, record_failures, "hot-single", v, "hot-default",
                    f"/{APP_NAME}", hot, "warm", rep=rep, hit_no=k,
                    expected_instance=hot_instance)

        client.close()
        server.stop()
        return {
            "scenario": "hot-single",
            "v": v,
            "port": port,
            "server_pid": server.pid,
            "reps": reps,
            "warmup_per_rep": warmup,
            "samples_per_rep": samples,
            "record_failures": record_failures[:RECORD_FAILURE_CAP],
            "records": records,
            "server_log_tail": log_tail(log_path),
        }
    finally:
        if server is not None:
            server.stop()
        if not cfg.get("keep"):
            shutil.rmtree(fixture, ignore_errors=True)


def run_churn(cfg: dict) -> dict:
    """Churn > pool LRU capacity, then readmission proven by process
    generation (instance id), with a warm control."""
    v: int = cfg["churn_versions"]
    if v <= POOL_LRU_CAPACITY:
        raise ValueError(
            f"churn versions ({v}) must be > pool LRU capacity ({POOL_LRU_CAPACITY})"
        )
    records: list = []
    record_failures: list = []
    instances: dict = {}
    server: ServerHandle | None = None
    fixture = Path(tempfile.mkdtemp(prefix="edger-version-bench-", dir="/tmp"))
    try:
        write_fixture(fixture, v)
        port = find_free_loopback_port()
        root_key = secrets.token_hex(16)
        log_path = fixture / "edger.log"
        log_path.touch()
        env = build_env(fixture, port, root_key, cfg["deno_bin"])
        server = ServerHandle(Path(cfg["edger_bin"]), env, fixture, log_path, fixture)
        base = f"http://127.0.0.1:{port}"
        wait_ready(base, READINESS_TIMEOUT_S)
        client = BenchClient("127.0.0.1", port)

        # Sweep all versions once: LRU (capacity 32) evicts the first (v - 32).
        # Each hit is a cold POINT (first spawn of that version).
        for i in range(v):
            version = version_str(i)
            hit(client, records, record_failures, "churn", v, "churn-sweep",
                f"/{APP_NAME}@{version}", version, "cold",
                rep=0, hit_no=i, instance_track=instances)

        sweep_instance = instances.get(HOT_VERSION, "")
        readmission = hit(
            client, records, record_failures, "churn", v, "readmission",
            f"/{APP_NAME}@{HOT_VERSION}", HOT_VERSION, "cold", rep=1, hit_no=0,
        )
        warm_control = hit(
            client, records, record_failures, "churn", v, "warm-control",
            f"/{APP_NAME}@{version_str(v - 1)}", version_str(v - 1), "warm", rep=1, hit_no=1,
        )
        post_warm = hit(
            client, records, record_failures, "churn", v, "post-readmission-warm",
            f"/{APP_NAME}@{HOT_VERSION}", HOT_VERSION, "warm", rep=1, hit_no=2,
        )

        # Recreation proof by generation: NOT by latency.
        recreation_ok = bool(
            sweep_instance
            and readmission["ok"]
            and readmission["instance"]
            and readmission["instance"] != sweep_instance
        )
        same_process_after = bool(
            readmission["instance"]
            and post_warm["ok"]
            and post_warm["instance"] == readmission["instance"]
        )
        control_ok = bool(
            warm_control["ok"]
            and instances.get(version_str(v - 1), "")
            and warm_control["instance"] == instances[version_str(v - 1)]
        )
        readmission_proven = recreation_ok and same_process_after and control_ok
        if not readmission_proven:
            record_failures.append(
                f"churn V={v} readmission not proven: recreation={recreation_ok} "
                f"same_process_after={same_process_after} warm_control={control_ok} "
                f"(sweep={sweep_instance[:8]!r} readmission={readmission['instance'][:8]!r} "
                f"post={post_warm['instance'][:8]!r} control={warm_control['instance'][:8]!r})"
            )

        client.close()
        server.stop()
        return {
            "scenario": "churn",
            "v": v,
            "port": port,
            "server_pid": server.pid,
            "pool_lru_capacity": POOL_LRU_CAPACITY,
            "evicted_range": f"1.0.0..1.0.{v - POOL_LRU_CAPACITY - 1}",
            "readmission_proven": readmission_proven,
            "readmission_ms": readmission["latency_ms"],
            "warm_control_ms": warm_control["latency_ms"],
            "record_failures": record_failures[:RECORD_FAILURE_CAP],
            "records": records,
            "server_log_tail": log_tail(log_path),
        }
    finally:
        if server is not None:
            server.stop()
        if not cfg.get("keep"):
            shutil.rmtree(fixture, ignore_errors=True)


def is_valid_sample(record: dict) -> bool:
    """A sample only counts for latency stats when the FULL contract held:
    status 200, no transport error, and no body/generation divergence."""
    return (
        record["ok"]
        and not record["error"]
        and not record["mismatch"]
        and not record.get("app_bad", False)
        and not record.get("empty_instance", False)
        and not record.get("divergent_generation", False)
    )


def aggregate(results: list) -> list:
    """Per (scenario, v, phase, class) aggregates over the ACTUAL samples.
    Warmup-class records are excluded (they are warmup, not data) and only
    fully valid samples (status/body/generation OK) feed the latency stats;
    the failure counters stay visible either way."""
    grouped: dict = {}
    for result in results:
        for record in result["records"]:
            if record["class"] == "warmup":
                continue
            key = (record["scenario"], record["v"], record["phase"], record["class"])
            grouped.setdefault(key, []).append(record)

    rows = []
    for (scenario, v, phase, cls), recs in sorted(grouped.items()):
        latencies = [r["latency_ms"] for r in recs if is_valid_sample(r)]
        stats = summarize(latencies)
        rows.append(
            {
                "scenario": scenario,
                "v": v,
                "phase": phase,
                "class": cls,
                "n": len(recs),
                "measured_ok": len(latencies),
                "errors": sum(1 for r in recs if (not r["ok"]) or r["error"]),
                "mismatches": sum(1 for r in recs if r["mismatch"]),
                "app_bad": sum(1 for r in recs if r.get("app_bad", False)),
                "empty_instance": sum(1 for r in recs if r.get("empty_instance", False)),
                "divergent_generation": sum(1 for r in recs if r.get("divergent_generation", False)),
                "retries_total": sum(r["retries"] for r in recs),
                "is_point": stats.get("is_point", False),
                "point_ms": stats.get("point_ms", ""),
                "p50_ms": stats.get("p50_ms", ""),
                "p95_ms": stats.get("p95_ms", ""),
                "p99_ms": stats.get("p99_ms", ""),
                "mean_ms": stats.get("mean_ms", ""),
                "min_ms": stats.get("min_ms", ""),
                "max_ms": stats.get("max_ms", ""),
            }
        )
    return rows


FIELDNAMES = [
    "scenario", "v", "phase", "class", "n", "measured_ok", "errors",
    "mismatches", "app_bad", "empty_instance", "divergent_generation",
    "retries_total", "is_point", "point_ms", "p50_ms",
    "p95_ms", "p99_ms", "mean_ms", "min_ms", "max_ms",
]


def write_results(out_dir: Path, results: list, meta: dict, rows: list) -> tuple:
    out_dir.mkdir(parents=True, exist_ok=True)
    json_path = out_dir / "results.json"
    csv_path = out_dir / "results.csv"

    with open(csv_path, "w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=FIELDNAMES)
        writer.writeheader()
        writer.writerows(rows)

    full = {
        "meta": meta,
        "summary_by_phase_class": rows,
        # Records are plain dicts (explicit fields, no object reprs);
        # aggregates above keep the per-class stats; the raw per-request
        # samples stay here so the real data is auditable.
        "scenarios": results,
    }
    json_path.write_text(json.dumps(full, indent=2) + "\n", encoding="utf-8")
    return json_path, csv_path


def parse_args(argv: list) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="version routing + pool HTTP benchmark")
    parser.add_argument(
        "--edger-bin",
        default=None,
        help="path to the edger binary (default: <repo>/target/release/edger)",
    )
    parser.add_argument(
        "--build",
        action="store_true",
        help="run 'cargo build --release -p edger-orchestrator' if the binary is missing",
    )
    parser.add_argument(
        "--deno-bin", default=None, help="Deno binary (default: $EDGER_DENO_BIN or PATH)"
    )
    parser.add_argument(
        "--v-list", default="1,10,100", help="hot-single V list (default 1,10,100)"
    )
    parser.add_argument(
        "--churn-versions",
        type=int,
        default=40,
        help=f"churn versions (> {POOL_LRU_CAPACITY}; default 40)",
    )
    parser.add_argument(
        "--reps",
        type=int,
        default=3,
        help="sequential hot reps per phase on the same server/process (default 3)",
    )
    parser.add_argument(
        "--warmup-per-rep",
        type=int,
        default=20,
        help="warmup hits per rep, excluded from statistics (default 20)",
    )
    parser.add_argument(
        "--samples-per-rep",
        type=int,
        default=60,
        help="measured hits per rep (default 60)",
    )
    parser.add_argument(
        "--out",
        default=None,
        help="results dir (default /tmp/edger-version-bench-results-<ts>)",
    )
    parser.add_argument(
        "--keep", action="store_true", help="keep /tmp fixtures after the run"
    )
    return parser.parse_args(argv)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def collect_provenance(repo_root: Path, edger_bin: Path, script_path: Path, deno_bin: str) -> dict:
    """Run provenance for the meta block: hashes of the binaries/scripts that
    produced the numbers, tool versions, git state (HEAD + dirty boolean only,
    no diff content), platform without hostname, and load average. No env
    dump, no secrets."""
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=str(repo_root), capture_output=True, text=True
    ).stdout.strip()
    dirty = bool(
        subprocess.run(
            ["git", "status", "--porcelain"], cwd=str(repo_root), capture_output=True, text=True
        ).stdout.strip()
    )
    try:
        deno_version = subprocess.run(
            [deno_bin, "--version"], capture_output=True, text=True, timeout=30
        ).stdout.splitlines()[0]
    except Exception:  # noqa: BLE001
        deno_version = "unknown"
    return {
        "sha256_edger_bin": sha256_file(edger_bin),
        "sha256_script": sha256_file(script_path),
        "deno_version": deno_version,
        "git_head": head or None,
        "git_dirty": dirty,
        "platform": f"{platform.system()} {platform.machine()}",
        "loadavg_before": list(os.getloadavg()),
    }


def main(argv: list) -> int:
    args = parse_args(argv)
    repo_root = Path(__file__).resolve().parent.parent
    edger_bin = Path(args.edger_bin) if args.edger_bin else repo_root / "target" / "release" / "edger"
    deno_bin = args.deno_bin or os.environ.get("EDGER_DENO_BIN", "") or shutil.which("deno")
    if not deno_bin:
        log("FATAL: Deno not found (pass --deno-bin or set EDGER_DENO_BIN / PATH)")
        return 1
    deno_bin = str(Path(deno_bin).expanduser())
    if not edger_bin.exists():
        if not args.build:
            log(f"FATAL: {edger_bin} not found; re-run with --build (or --edger-bin)")
            return 1
        log(f"building {edger_bin} ...")
        build = subprocess.run(
            ["cargo", "build", "--release", "-p", "edger-orchestrator"], cwd=str(repo_root)
        )
        if build.returncode != 0 or not edger_bin.exists():
            log("FATAL: cargo build failed")
            return 1
    if args.reps < 1 or args.samples_per_rep < 20 or args.warmup_per_rep < 1:
        log("FATAL: reps >= 1, samples-per-rep >= 20, warmup-per-rep >= 1")
        return 1
    v_list = [int(part) for part in args.v_list.split(",") if part.strip()]
    if not v_list:
        log("FATAL: empty --v-list")
        return 1

    out_dir = (
        Path(args.out)
        if args.out
        else Path(tempfile.mkdtemp(prefix="edger-version-bench-results-", dir="/tmp"))
    )
    # Provenance captured BEFORE the run: the exact binaries/script that
    # produce the numbers must be identified (no env dump, no secrets).
    provenance = collect_provenance(
        repo_root, edger_bin, Path(__file__).resolve(), deno_bin
    )

    cfg = {
        "edger_bin": str(edger_bin),
        "deno_bin": deno_bin,
        "keep": args.keep,
        "reps": args.reps,
        "warmup_per_rep": args.warmup_per_rep,
        "samples_per_rep": args.samples_per_rep,
    }
    results: list = []
    scenario_failures: list = []

    def run_safely(name: str, fn) -> None:
        try:
            result = fn()
            results.append(result)
            log(f"{name}: done")
        except Exception as err:  # noqa: BLE001 - record and continue
            scenario_failures.append(f"{name}: {err}")
            log(f"{name}: FAILED ({err})")

    try:
        for v in v_list:
            cfg["v"] = v
            run_safely(f"hot-single V={v}", lambda: run_hot_single(cfg))
        cfg["churn_versions"] = args.churn_versions
        run_safely(f"churn V={args.churn_versions}", lambda: run_churn(cfg))
    except KeyboardInterrupt:
        log("interrupted — cleaning up and writing partial results")

    rows = aggregate(results)
    record_failures = [
        item for result in results for item in result.get("record_failures", [])
    ]
    meta = {
        "benchmark": "version-routing-http",
        "app": APP_NAME,
        "repo_root": str(repo_root),
        "edger_bin": str(edger_bin),
        "deno_bin": deno_bin,
        "config": {
            "v_list": v_list,
            "churn_versions": args.churn_versions,
            "reps": args.reps,
            "warmup_per_rep_excluded_from_stats": args.warmup_per_rep,
            "samples_per_rep": args.samples_per_rep,
            "pool_lru_capacity": POOL_LRU_CAPACITY,
            "units": "ms per HTTP request round-trip (loopback, real Deno processes)",
            "reserved_ports_refused": sorted(RESERVED_PORTS),
            "explicitly_disabled_optins": [
                "EDGER_TENANT_ROUTING_ENABLED",
                "EDGER_WEIGHTED_ROUTING_ENABLED",
                "EDGER_OTEL_ENABLED",
                "OTEL_EXPORTER_OTLP_ENDPOINT",
            ],
            "forced_env": {
                "EDGER_JS_RUNTIME": "process",
                "note": "only 'bridge' switches the JS runtime (bin/edger.rs); forced so the benchmark never inherits an operator value",
            },
            "readmission_proof": "process generation (instance id), not latency",
        },
        "scenario_failures": scenario_failures,
        "record_failures": record_failures[:RECORD_FAILURE_CAP],
        "record_failure_count": len(record_failures),
        "provenance": provenance,
        "loadavg_after": list(os.getloadavg()),
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    }
    json_path, csv_path = write_results(out_dir, results, meta, rows)
    log(f"results: {json_path}")
    log(f"results: {csv_path}")
    # Non-zero exit whenever ANY scenario failed OR any request came back
    # non-200 / mismatched / with a failed readmission proof.
    return 1 if (scenario_failures or record_failures) else 0


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_args: sys.exit(130))
    sys.exit(main(sys.argv[1:]))
