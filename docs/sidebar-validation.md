# Home tree regression checks

Use the real App with mocked Tauri IPC in a fresh browser profile. Enable CDP focus emulation
(`Emulation.setFocusEmulationEnabled`) for headless keyboard checks: a background headless page can
change `activeElement` without delivering focus events. Reject unknown IPC rather than returning
success for everything. This harness does not launch wtm or access its live data.

The fixture needs at least 30 sessions across three projects: consecutive idle sessions, a busy
session, an unread completion, a failed empty restore, a Home-opened session, a shell run, a long
issue-key slug, main and detached checkouts, and a worktree with no sessions.

- Search full branch/path metadata, including a collapsed no-session worktree. Clear the filter.
  Completed includes finished turns already seen; Failed has its own filter.
- Idle siblings fold in consecutive groups of three or more, preserving session order when status
  changes. Attention, failed and unread completion rows survive ancestor folds. Selected and
  keyboard-focused rows survive idle folds. A focused row's removal lands on a surviving ancestor.
- Check Up/Down, Home/End, Left/Right, Enter/Space, and Shift+F10. Exactly one tree row is a tab stop.
  Command-F in the Home composer or an editor must leave that control in charge of search.
- Each row has at most one status dot. Every marker has the same x coordinate across levels.
  Counts have `scrollWidth <= clientWidth`, and the panel has no horizontal overflow. Issue keys
  stay whole; at narrow widths/large text they can occupy a separate line below the controls.
- Exercise 860/1280 px windows and 200/320/420 px sidebars in light/dark, standard/compact, and 200%
  text. Density changes row spacing, not font size. Reload preserves density and folds.
- Keep an exchange in flight: no wire SVG or per-row message chip is mounted. Activity and In flight
  still show its message. Home provenance appears in the row tooltip/accessibility description and
  selected Peek detail. Open a shell row into the actual terminal, not a second Peek transcript.

The implementation's mocked checks covered identity fallbacks (6 cases), structural tree behavior
(9 cases), and the full-App layout/focus/filter checks. The no-wire assertion failed on the old tree,
which mounted one overlay for the same in-flight fixture. Native menu tracking and real restore,
terminal, and assistive-technology behavior remain part of the release click-through.
