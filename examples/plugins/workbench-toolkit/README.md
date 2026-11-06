# Workbench Toolkit

This is a real API 2 author example, not a built-in product catalog. Review its local folder
from Settings → Plugins → Add plugin. Python 3 is required only for the runtime example.
Nothing installs automatically. The runtime does not use credentials or the network.

The package styles existing Chat links, adds message/panel/workspace actions, inserts content
and Appearance settings, and observes workspace/panel events. Its message action opens WTM's
real edit review. Exact turn-boundary editing currently requires an installed Codex CLI that
advertises `ThreadForkParams.lastTurnId`. Unsupported providers, attachment items, in-turn
steers, stale history and busy sessions report an explanation without changing the original.

Use the in-app contract inventory and `docs/plugins.md` for other targets and operations.
Uninstall leaves user settings and durable provider conversations intact.
