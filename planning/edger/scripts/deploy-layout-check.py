#!/usr/bin/env python3
"""Validate the local operation/deploy layout documentation contract."""

from __future__ import annotations

import argparse
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Check:
    name: str
    path: str
    tokens: tuple[str, ...]


CHECKS = (
    Check(
        name="runtime roots and launch command",
        path="docs/developers/06-operacao-e-testes.adoc",
        tokens=(
            "RUNTIME_WORKER_DIRS",
            "workers",
            "cargo run -p edger-orchestrator --bin edger",
        ),
    ),
    Check(
        name="local state and auth files",
        path="docs/developers/06-operacao-e-testes.adoc",
        tokens=(
            "EDGER_AUTH_DB",
            "EDGER_STATE_DIR",
            "EDGER_EXTENSION_STATUS_FILE",
            "$EDGER_STATE_DIR/extension-status.json",
        ),
    ),
    Check(
        name="durable provider boundaries",
        path="docs/developers/06-operacao-e-testes.adoc",
        tokens=(
            "EDGER_DURABLE_SQL_PROVIDER",
            "turso-remote",
            "turso-sync",
            "EDGER_TURSO_URL",
            "EDGER_TURSO_AUTH_TOKEN",
        ),
    ),
    Check(
        name="health and metrics probes",
        path="docs/developers/06-operacao-e-testes.adoc",
        tokens=(
            "/healthz",
            "/readyz",
            "/livez",
            "/metrics",
        ),
    ),
    Check(
        name="local backup and restore",
        path="docs/developers/06-operacao-e-testes.adoc",
        tokens=(
            "Backup local",
            'cp "$EDGER_AUTH_DB"',
            'cp -R "$EDGER_STATE_DIR"',
            'cp "$EDGER_EXTENSION_STATUS_FILE"',
            "Restauração local",
        ),
    ),
    Check(
        name="admin and gateway operations",
        path="docs/developers/06-operacao-e-testes.adoc",
        tokens=(
            "/api/admin/session",
            "/api/admin/workers",
            "/api/admin/extensions",
            "/api/admin/gateway/stats",
            "/api/admin/gateway/rate-limit/metrics",
        ),
    ),
    Check(
        name="operational troubleshooting",
        path="docs/developers/06-operacao-e-testes.adoc",
        tokens=(
            "/readyz",
            "Estado some após restart",
            "Status de extensão volta",
            "EDGER_DENO_BIN",
        ),
    ),
    Check(
        name="required gates documented",
        path="docs/developers/06-operacao-e-testes.adoc",
        tokens=(
            "cargo test --workspace",
            "cargo clippy --workspace -- -D warnings",
            "cargo fmt -- --check",
            "planning/edger/scripts/run-gates.sh",
            "planning/edger/scripts/deploy-layout-check.py",
        ),
    ),
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--repo",
        default=".",
        help="Repository root. Defaults to the current working directory.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    repo = Path(args.repo).resolve()
    failures: list[str] = []

    for check in CHECKS:
        path = repo / check.path
        if not path.exists():
            failures.append(f"{check.name}: missing file {check.path}")
            continue
        text = path.read_text(encoding="utf-8")
        missing = [token for token in check.tokens if token not in text]
        if missing:
            failures.append(f"{check.name}: missing {', '.join(missing)}")
            continue
        print(f"OK {check.name}")

    if failures:
        for failure in failures:
            print(f"FAIL {failure}")
        return 1

    print(f"PASS deploy layout checks: {len(CHECKS)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
