# Bot Crossing rendering code

The files in this directory are adapted from Bot Crossing at commit
`ec668f41946ad2fa7871c83436bebb2e6de29742`:

https://github.com/jarrenrocks/bot-crossing

They retain Bot Crossing's module boundaries so fixes can be compared with upstream. WTM owns the
typed state projection, Svelte interface, and lifecycle wrapper outside this directory. Bot
Crossing's server, session scanners, polling, archive state, deep links, and DOM HUD are not
vendored.

Bot Crossing is Copyright (c) 2026 Jarren Rocks and is licensed under the MIT License. The complete
license is in `BOT_CROSSING_LICENSE` beside this file.

The GLB files under `public/assets/game/` were built from Kay Lousberg's CC0 KayKit assets. Their
credits and source links are in `public/assets/game/CREDITS.md`.

The floating status badges import eight path-data exports from `@mdi/js`. The package notice is
included in `MDI_LICENSE` beside this file.
