# Workbench Toolkit

This is a real API 2 author example, not a built-in product catalog. Review its local folder
from Settings → Plugins → Add plugin. Nothing installs automatically. The runtime does not use
credentials or the network.

The package styles existing Chat links, adds message, panel and worktree actions, inserts content
and an Appearance setting, and observes worktree and panel events. Its message action opens wtm's
real edit review. Exact turn-boundary editing currently requires an installed Codex CLI that
advertises `ThreadForkParams.lastTurnId`. Unsupported providers, attachment items, in-turn
steers, stale history and busy sessions report an explanation without changing the original.

Its runtime, `python3 runtime.py`, starts as soon as the package is enabled, so it needs Python 3
on wtm's PATH. Without it, the package's page in Settings → Plugins shows the runtime error and
**Check toolkit runtime** fails; the styles, content, setting and the other actions do not use the
runtime and keep working.

Because it declares a runtime and host access, the review marks it as native code. Use the
plugin guide and **Develop a plugin → Show the public contract** for other targets and
operations. Uninstalling leaves its settings and any provider conversations intact.
