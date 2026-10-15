#!/usr/bin/env python3
"""Reproduce the narrow bundled Open Design subset from verified upstream source."""
import hashlib
import json
from pathlib import Path
import shutil
import sys

output = Path(__file__).resolve().parent.parent / "vendor/open-design"
manifest = json.loads((output / "provenance.json").read_text())
source = Path(sys.argv[1])
for name, expected in manifest["sources"].items():
    actual = hashlib.sha256((source / name).read_bytes()).hexdigest()
    if actual != expected:
        raise SystemExit(f"Upstream source differs from the pinned revision: {name}")
charter = (source / "apps/daemon/src/prompts/core-slim.ts").read_text()
parts = [
    charter[charter.index("## Role\n"):charter.index("## Task Types and Standards")],
    charter[charter.index("### 2. Plan"):charter.index("5. **Inspect the rendered result")],
    charter[charter.index("## Artifact Refinement Phase"):charter.index("${HANDOFF_PLACEHOLDER}", charter.index("## Artifact Refinement Phase"))],
]
text = "\n\n".join(part.strip() for part in parts).replace("\\`", "`")
(output / "design-charter.md").write_text(
    "<!-- Adapted by WTM: excerpted reusable charter; filesystem, form, imagery and rendering delivery use the WTM host contract. Apache-2.0. -->\n"
    + text + "\n"
)
shutil.copyfile(source / "LICENSE", output / "LICENSE")
shutil.copyfile(source / "apps/daemon/src/design-systems/token-evidence.ts", output / "token-evidence.ts")
