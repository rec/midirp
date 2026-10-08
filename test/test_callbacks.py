"""Bound native callback tests so a GIL deadlock fails instead of hanging pytest."""

import json
import os
import subprocess
import sys
from pathlib import Path


def test_native_callback_lifecycle_completes_without_deadlock() -> None:
    project = Path(__file__).resolve().parents[1]
    environment = {
        "PATH": os.environ["PATH"],
        "PYO3_PYTHON": sys.executable,
        "CARGO_HOME": str(project / "target" / "cargo-home"),
    }
    build = subprocess.run(
        ["cargo", "test", "--lib", "--no-run", "--message-format=json"],
        cwd=project,
        env=environment,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
        timeout=60,
    )
    artifacts = [
        j["executable"]
        for x in build.stdout.splitlines()
        if (j := json.loads(x))["reason"] == "compiler-artifact"
        and j["profile"]["test"]
        and j["executable"] is not None
    ]
    (executable,) = artifacts
    subprocess.run(
        [executable, "callback::tests", "--test-threads=1"],
        cwd=project,
        env=environment,
        check=True,
        timeout=15,
    )
