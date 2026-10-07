#!/usr/bin/env python3
"""Run only the already-built, isolated example; kill only its child PID on timeout."""

import pathlib
import os
import subprocess
import sys
import tempfile

root = pathlib.Path(__file__).resolve().parent.parent
binary = root / "target/debug/examples/native_menu_probe"
if not binary.is_file():
    sys.exit("Build first: cargo build -p wtm-app --example native_menu_probe")
with tempfile.TemporaryDirectory(prefix="wtm-native-probe-") as isolated:
    # Do not inherit the watching agent's bridge credentials. The isolated builder
    # never uses them, but this also protects a future addition to the probe.
    env = {key: value for key, value in os.environ.items() if not key.startswith("WTM_")}
    for key, name in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("TMPDIR", "tmp")]:
        directory = pathlib.Path(isolated, name)
        directory.mkdir()
        env[key] = str(directory)
    with subprocess.Popen([str(binary)], cwd=root, env=env) as probe:
        try:
            code = probe.wait(timeout=60)
        except (subprocess.TimeoutExpired, KeyboardInterrupt):
            probe.kill()
            probe.wait()
            sys.exit("Stopped only the isolated probe. A timeout on the old route is expected.")
        sys.exit(code)
