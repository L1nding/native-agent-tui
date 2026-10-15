"""Pause real Blender before its unchanged production script for one exact test job."""

import hashlib
import json
import os
import sys
import time
from pathlib import Path

import bpy

settings = json.loads(Path(os.environ["MODELING_PROJECT_CANCEL_GATE"]).read_bytes())
context = Path(settings["context_path"]).resolve()
assert hashlib.sha256(context.read_bytes()).hexdigest() == settings["context_sha256"]
root = Path(json.loads(context.read_bytes())["project_root"]).resolve()
assert context.is_relative_to(root / "outputs/domain")
arguments = sys.argv[sys.argv.index("--") + 1 :]
output = Path(arguments[1]).resolve()
expected = context.parent / "project/building" / settings["build_id"] / "model.glb"
assert output == expected, "test gate cannot affect another job"
(output.parent / "real-blender-cancel-gate.json").write_text(
    json.dumps(
        {
            "pid": os.getpid(),
            "binary": bpy.app.binary_path,
            "version": bpy.app.version_string,
            "output": str(output),
            "time_ns": time.time_ns(),
            "controlled_pause": True,
        }
    ),
    encoding="utf-8",
)
deadline = time.monotonic() + 150
while time.monotonic() < deadline:
    time.sleep(0.025)
raise TimeoutError(
    "controlled cancellation did not terminate actual Blender before export"
)
