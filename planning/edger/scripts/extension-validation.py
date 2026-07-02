#!/usr/bin/env python3
"""Run the local extension/module validation contract.

This gate is intentionally local-only: it validates extension inventory,
manifest, status, diagnostics and redaction through a targeted Rust integration
test without external network, deploy or marketplace access.
"""

from __future__ import annotations

import argparse
import pathlib
import subprocess
import sys


TEST_NAME = (
    "local_extension_validation_contract_reports_manifest_status_diagnostics_and_redaction"
)
TEST_COMMAND = [
    "cargo",
    "test",
    "-p",
    "edger-orchestrator",
    "--test",
    "admin_workers_plugins",
    TEST_NAME,
    "--",
    "--exact",
]
REQUIRED_CONTRACT_FILES = [
    "edger-core/src/admin.rs",
    "edger-orchestrator/src/registry.rs",
    "edger-orchestrator/tests/admin_workers_plugins.rs",
    "docs/developers/06-operacao-e-testes.adoc",
    "planning/edger/docs/value-parity-matrix.md",
]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default=".", help="repository root")
    parser.add_argument(
        "--module",
        default="gateway",
        help="extension module to validate; v1 supports gateway",
    )
    args = parser.parse_args()

    repo = pathlib.Path(args.repo).resolve()
    if args.module != "gateway":
        print(f"FAIL unsupported module for local validation: {args.module}")
        return 2

    print(f"extension-validation repo={repo}")
    print(f"extension-validation module={args.module}")

    missing = [path for path in REQUIRED_CONTRACT_FILES if not (repo / path).exists()]
    if missing:
        for path in missing:
            print(f"FAIL missing contract file: {path}")
        return 1

    print("extension-validation required contract files: ok")
    print("extension-validation command: " + " ".join(TEST_COMMAND))
    result = subprocess.run(
        TEST_COMMAND,
        cwd=repo,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )
    print(result.stdout, end="")
    if result.returncode != 0:
        print(f"FAIL extension validation test exited with {result.returncode}")
        return result.returncode

    print(
        "PASS extension validation: manifest/status/diagnostics/redaction are locally validated"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
