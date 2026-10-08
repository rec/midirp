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
        check=False,
        stdout=subprocess.PIPE,
        text=True,
        timeout=60,
    )
    messages = [json.loads(x) for x in build.stdout.splitlines()]
    assert build.returncode == 0, "\n".join(
        x["message"]["rendered"] for x in messages if x["reason"] == "compiler-message"
    )
    artifacts = [
        x["executable"]
        for x in messages
        if x["reason"] == "compiler-artifact"
        and x["profile"]["test"]
        and x["executable"] is not None
    ]
    (executable,) = artifacts
    subprocess.run(
        [executable, "callback::tests", "--test-threads=1"],
        cwd=project,
        env=environment,
        check=True,
        timeout=15,
    )

    for t in (
        "callback::tests::interpreter_finalization_drains_a_live_input",
        "lifecycle::tests::shutdown_rejects_new_registrations_and_is_idempotent",
    ):
        subprocess.run(
            [executable, t, "--exact", "--ignored", "--test-threads=1"],
            cwd=project,
            env=environment,
            check=True,
            timeout=15,
        )
