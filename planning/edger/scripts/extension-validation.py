#!/usr/bin/env python3
"""Validate the current minimal-runtime extension boundary.

Epic 17 removed the in-process extension registry and gateway module from the
supported runtime. This local-only gate prevents those obsolete control-plane
surfaces from returning and verifies that the published compatibility matrix
keeps the removals explicit.
"""

from __future__ import annotations

import argparse
import pathlib
import subprocess
import sys


TEST_NAME = "known_removed_rows_remain_explicit"
TEST_COMMAND = [
    "cargo",
    "test",
    "-p",
    "edger-orchestrator",
    "--test",
    "compat_matrix",
    TEST_NAME,
    "--",
    "--exact",
]
REQUIRED_CONTRACT_FILES = [
    "edger-orchestrator/tests/compat_matrix.rs",
    "planning/edger/epics/17-edger-minimalista/00-overview.md",
    "planning/edger/docs/compat-matrix.md",
    "planning/edger/docs/value-parity-matrix.md",
]

REMOVED_RUNTIME_FILES = [
    "edger-orchestrator/src/registry.rs",
    "edger-orchestrator/tests/admin_workers_plugins.rs",
]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default=".", help="repository root")
    parser.add_argument(
        "--module",
        default="gateway",
        help="legacy module name retained for CLI compatibility",
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

    returned = [path for path in REMOVED_RUNTIME_FILES if (repo / path).exists()]
    if returned:
        for path in returned:
            print(f"FAIL removed runtime surface returned: {path}")
        return 1

    print("extension-validation current boundary files: ok")
    print("extension-validation removed runtime surfaces: absent")
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
        "PASS extension validation: minimal-runtime removals remain explicit and tested"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
