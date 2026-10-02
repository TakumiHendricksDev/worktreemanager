<img src="assets/brand/wtm-icon.svg" alt="" width="72">

# wtm — Worktree Manager

A macOS app for working in several git worktrees at once, with coding agents doing much of the work.
Each worktree gets its own surface of panes: Claude Code, Codex and Cursor Agent sessions, shells, and
real browser pages, tiled side by side. Beside them sit a Database view and a Code view for checking
what the agents did. New worktrees come from a **New Worktree** form that each project defines for
itself in a `wtm.toml`, including dropdowns filled by your own shell commands (branch lists, Jira
issues via `acli`, anything that prints to stdout).

The app knows nothing about `just`, Jira, Docker, or any particular repo. It reads git, runs the
commands your config declares, and drives the agent CLIs you already have installed. A repo with heavy
worktree tooling and a bare library with none are the same code path with different TOML.

Built with Tauri v2 + Rust + Svelte 5.

**Status:** a personal tool, shared through a Homebrew tap. macOS 13+, Apple silicon, signed and
notarized `.app`.

### At a glance

**Worktrees**

- Register any git repository and its worktrees are listed down the left. Filter them, drag them into
  order, file them into named groups that fold away, and star the ones you want in Favorites.
- **New Worktree** builds a form from the project's config and shows a review screen with the *exact*
  `git worktree add` and setup argv before anything runs. Then it runs preflight checks, creates the
  worktree, and runs the project's setup command in a live terminal. You can adopt an existing branch
  instead of creating one.
- **Remove Worktree** runs the project's teardown steps, then `git worktree remove`, then optionally
  deletes the branch.
- **Details** (⌘I) shows the path, branch, HEAD, working-tree state, ahead/behind, a port table from
  config, and the worktree's `.env` keys, each value masked until you reveal it.

**Agents**

- Live **Claude Code**, **Codex** and **Cursor Agent** sessions. Each has a streamed transcript,
  approval cards, model, effort and mode pickers, file attachments, message queueing and mid-turn
  steering, `/btw` side questions, and resumable conversations. Claude also has its fast mode.
- An agent can **delegate** to another, either one child or a run of up to 20. An agent that **runs
  out of usage** can hand its work to another provider.
- Every session gets **wtm's own MCP tools**: delegation, the worktree's browser panes, and the
  comments you leave in the Code tab.
- **Usage limits** for every agent account, in one dialog.
- **Attention**: status dots, the dock badge, in-app cards, and macOS notifications when a session
  needs you.
- **Voice dictation** into the composer. It's opt-in and uses Deepgram.

**Views**

- **Code tab**: read a worktree's files with real syntax highlighting, search across them, review just
  what the branch changed, and leave line comments for an agent.
- **Database view**: PostgreSQL and SQLite, with a selectable and copyable grid, WHERE / ORDER BY
  filtering, a SQL console, and a Run button on SQL in an agent's reply.
- **Browser panes**: a real web page beside your sessions, which the worktree's agents can drive, with
  element-anchored comments you can hand to an agent.
- **Pop a pane out**: any pane can move into a window of its own and back.
- **Open in …** hands the worktree to your editor, a terminal, Fork, Finder, or Claude.

**Not done yet**

- 🚧 A command palette, and file-watching auto-refresh. The worktree list refreshes on window focus,
  on ⌘R, and after a create or remove.
- 🚧 Buttons for `[[action]]`s. The config still accepts and validates them, but the current window
  has nowhere to run one.
- 🚧 `[remove] strategy = "command"`. The native path is the default, and it's the one that turns the
  branch prompt into a checkbox.

Verified end to end against a real repository, not just against fakes. Point the suite at your own
checkout and it creates a worktree (real issue-tracker lookup, real branch naming, real
`git worktree add`), asserts it against git, then removes it and deletes the branch:

```bash
WTM_TEST_REPO=~/code/myproject cargo test -p wtm-app -- --ignored --nocapture --test-threads=1
```

Every expectation there is derived from *your* config, so it verifies wtm rather than any one
project's convention. With the variable unset the tests skip.

---

## Contents

[Install](#install) · [Updating](#updating-an-install-you-already-have) ·
[The window](#the-window) · [Home](#home) · [Agent sessions](#agent-sessions) ·
[Worktrees](#worktrees) ·
[Registering a project](#registering-a-project) · [Writing wtm.toml](#writing-wtmtoml) ·
[Database viewer](#database-viewer) · [Code tab](#code-tab) · [Browser panes](#browser-panes) ·
[Panes in their own windows](#panes-in-their-own-windows) · [Open in …](#open-in-) ·
[Environment values](#environment-values) · [Settings](#settings) ·
[Keyboard shortcuts](#keyboard-shortcuts) · [Prerequisites](#prerequisites) · [Setup](#setup) ·
[First run](#first-run) · [Dev workflow](#dev-workflow) · [Build & install](#build--install) ·
[Troubleshooting](#troubleshooting) · [Logs](#logs) · [Dependencies](#dependencies) ·
[Architecture](#architecture)

---

## Install

You do not need to clone this repository to use wtm. Everything below is
[the latest release](https://github.com/TakumiHendricksDev/worktreemanager/releases/latest).

### macOS (Apple silicon, 13+)

```bash
brew install --cask takumihendricksdev/tap/wtm
```

wtm is **signed with a Developer ID and notarized by Apple**, so Gatekeeper accepts it
like any other downloaded app.

The [cask](https://github.com/TakumiHendricksDev/homebrew-tap) also clears the quarantine
attribute after installing. Earlier releases were unsigned, and for those that step was the
only reason the app opened at all; macOS called them *"damaged and can't be opened"*. For a
notarized release it only skips the one-time *"downloaded from the Internet"* confirmation.
If you would rather see that confirmation, take the zip below instead of the tap.

wtm drives agent CLIs rather than shipping one. For agent sessions, install whichever of
`claude`, `codex` and `cursor-agent` you use and sign in to it as usual. wtm finds them on your
login shell's `PATH`.

### Updating an install you already have

```bash
brew update && brew upgrade --cask wtm
```

`brew update` is not optional here — it is what fetches the tap's new cask. Without it
Homebrew still holds the recipe it last saw and will report wtm as up to date whatever
has been released. The upgrade backs up the old `.app`, swaps in the new one, and clears
the quarantine attribute again, so the new version opens without asking.

`brew list --cask --versions wtm` says which version you actually have, which is worth
checking against the [latest release](https://github.com/TakumiHendricksDev/worktreemanager/releases/latest)
if a feature you expect is missing.

**wtm tells you when there is a new version.** At launch, and at most once a day, it asks
GitHub for the latest release; the request carries nothing about you or your machine. A
banner offers the update, and *Update and restart* runs the commands above for you. It
downloads first, while wtm is still open, then quits, upgrades, and reopens. Running agent
turns and terminals stop, and your panes come back. **Check for Updates…** in the Worktree
Manager menu asks on demand, *Skip this version* stops the automatic check offering that
release again, and Settings → General turns the automatic check off.

A copy that did not come from the tap is told about the release but not updated, since
Homebrew has nothing to upgrade.

To remove it: `brew uninstall --cask wtm`, or `brew uninstall --zap --cask wtm` to take
`~/.config/wtm` (your preferences, trust decisions and log) with it.

### Or the raw artifacts

The zip is on the [releases page](https://github.com/TakumiHendricksDev/worktreemanager/releases)
with a SHA-256 checksum. A browser download is fine: the app is notarized, so macOS asks
once whether to open something downloaded from the Internet, and that's all.

---

## The window

The window has three parts: the title bar, the sidebar of worktrees, and the selected worktree's
surface.

**Title bar.** From left to right:

- The sidebar toggle (⌃⌘S).
- **Home** (⇧⌘H), with the number of approvals waiting anywhere. See [Home](#home).
- The project picker. It switches repositories, has **Add a repository…**, and has **Remove "name"
  from wtm…**, which forgets a repository without touching anything on disk. A ● marks a project with
  a session waiting on you.
- On the right: **Usage limits**, a theme button that cycles system → light → dark, and **Settings**
  (⌘,).

**Sidebar.** One row per worktree:

- **Title**: the directory name, unless `display.title` says otherwise.
- **Subtitle**: the branch, unless `display.subtitle` says otherwise.
- A `main` pill on the main worktree.
- **A third line**, when there is something to say: a session's status (*needs you*, *failed*,
  *done*, *working…*), *● modified*, or *stale* for a worktree whose directory has gone.

Working with the list:

- **⌘F filters it.** Space-separated terms are matched against the title, branch, directory, and an
  issue key found in the branch or directory name.
- **Drag rows** to reorder them, or into named groups.
- **Right-click** a row or a heading for the rest: Favorites, Move To, New Group…, Rename…, Collapse
  All.
- **⌥↑ / ⌥↓** moves the focused row or group.
- **A folded group** still shows the selected worktree, and any worktree whose session needs you or
  failed.
- **The layout is saved per project** in `~/.config/wtm/config.toml`.
- **+ New Worktree** is at the bottom.

**Worktree bar.** Across the top of the surface: a star, the worktree's name, the branch and any
configured badges. The name becomes a switcher menu when the sidebar is hidden. Then:

- **Sessions | Database | Code**: what the surface shows.
- **New ▾**: an agent, a **Shell** (⌘J), or a **Browser**. Once an agent's usage is known, its row
  says how close it is to its limit.
- **Open in …**: see [below](#open-in-).
- **Links**: a split button, shown when the project's config declares links. It keeps the one you
  opened last a click away.
- **⋯**: **Details…** (⌘I) and **Remove Worktree…**.

**Panes.** Every session is a pane, tiled per worktree.

- **Placement.** Agents and browsers open to the right of the focused pane, shells below it.
- **Moving.** Drag a pane by the grip at the left of its header. Drop it on another pane's edge to put
  it beside that pane, in the middle to swap the two, or on the ring around the surface to run it
  along that side.
- **Resizing.** Drag a divider, or focus it and use the arrow keys.
- **Header buttons.** Restart, Split (another pane of the same kind), Pop out, and Close. A narrow
  pane folds the first three into ⋯.
- **Limits.** 20 panes per worktree, and 40 in all.

An empty worktree offers a button per agent, plus Shell and Browser. Below them it lists the
conversations you can pick up again, the plans you have saved, and Claude's background agents.

After a relaunch the panes come back where they were, and fill themselves the first time you look at
their worktree. Shells start again, browsers reload, and agent panes resume their conversations in
place. Resuming starts the agent's CLI; no message is sent until you write one. An agent pane with nothing to
resume, or whose resume fails, closes, and a banner says why. Its conversation is still under **Pick
up where you left off**.

## Home

Home is a view that isn't any one worktree. Open it with the house in the title bar, or Go › Home
(⇧⌘H); the same again goes back to where you were. wtm reopens on Home if you quit from it.

- **The session tree** takes the sidebar's place: every project, its worktrees, and every agent
  session in them, with delegated children nested under the session that started them. Each says
  what it is doing — *working…*, *needs you*, *done*, *failed* — and a folded project still shows any
  session that needs you or failed. The counts at the top filter it. **+** on a worktree starts an
  agent there, as an ordinary pane in that worktree.
- **Needs you** lists every approval waiting in any session, oldest first, and you answer each on the
  same card the pane would have shown, without going there.
- **Peek** shows the session you picked in the tree: its transcript, what it is waiting on, and a box
  to send it a message or queue one. **Open in worktree** takes you to its pane.
- **Wires.** While one agent is waiting on another — a delegation, or a message from Home — a line
  lights up between them in the tree, with what was asked on the receiving row. **Activity** keeps the
  recent ones.
- **The Home agent.** Start Claude, Codex or Cursor in Home's main column. It runs in a folder of its
  own, outside every repository, and has tools for the sessions everywhere else: list them, read one,
  message one and wait for the answer, start one in any worktree of any project, stop a turn it
  started, close sessions it opened that you haven't used, and create a worktree when you ask — it
  previews the form's plan first, and anything the preview calls an error stops it, since only you
  can override one. Its setup runs in Home, in a terminal you can type into. It cannot answer
  approvals; those come
  to you under Needs you. Messages it sends arrive labelled *From Home (wtm)*. **History** picks up a
  past Home conversation.

## Agent sessions

wtm drives the agent CLIs you already have. It does not ship a model or proxy one, and each agent
signs in and bills exactly as it does in your terminal.

| | Claude Code | Codex | Cursor Agent |
|---|---|---|---|
| Driven over | `claude -p` stream-json | `codex app-server` | `cursor-agent acp` (Agent Client Protocol) |
| Models | a list compiled into this build; any other id can be set in `wtm.toml` | listed live by Codex | listed live by Cursor |
| Effort | low → max, plus ultracode; applies on restart | per model; applies live | Cursor's thought level; applies live |
| Modes | Manual, Accept edits, Plan, **Auto**, Don't ask, Bypass permissions | Read only, **Auto**, Full access | Agent, **Auto**, Plan, Ask |
| Fast mode | ✅ | — | — |
| Steer a running turn | ✅ | ✅ | stops the turn, then sends |
| Resume a conversation | ✅ | ✅ | ✅ |

An agent is offered in **New ▾** when its CLI is on wtm's `PATH` and the repository hasn't turned it
off. Cursor's CLI is found as `cursor-agent`, or as the copy inside Cursor.app. An empty worktree's
launcher lists every agent, and greys out one that can't start, with the reason.

**Auto by default.** A new session starts in Auto unless the picker, the repository's config, or the
chosen model says otherwise. It also starts at effort xhigh where the model has it. Auto means each
provider's own version: Claude's permission classifier, or Codex's auto-reviewer. Cursor has no such
mode, so its Auto is wtm answering every request with "allow once". That puts nothing between the model
and the machine, so pick Agent or Ask for a Cursor session you want to approve step by step.

**Approvals** arrive as cards in the transcript, one at a time:

- *Run this command?*, *Apply this change?* and *Grant these permissions?* get **Allow**, **Always this
  session**, and **Deny**. After a Deny, the turn carries on without it.
- **Questions** get their answers to pick, plus a free-text Other.
- *Approve this plan?* lets you read the plan beside the transcript, send it back with **Request
  changes**, or approve it. An approved plan lets the session carry on in Auto, and the plan is saved
  to the worktree's **Plans**.
- A card that has scrolled out of view leaves a *Jump to request* button.

**The composer.**

- **Sending and queueing.** ⌘↵ sends; Settings can make a bare ↵ send instead. While a turn runs,
  Send becomes **Queue**. Queued messages go one per finished turn, and each can be edited, removed,
  or sent now. ⇧⌘↵ steers a message straight into the running turn.
- **Completion.** `@` completes file names from the worktree, and `/` completes the session's skills
  and commands.
- **Attachments.** Attach files from **+**, by pasting, or by dropping them from Finder, up to 20 MB.
  Images reach the model as images; other files as their paths.
- **Context.** The **Context** ring opens a card showing the context window, the last turn's tokens,
  and this agent's usage limits.
- **Commands wtm handles itself:**
  - `/clear` or `/new` restarts the pane.
  - `/context` (also `/status` and `/usage`) opens the context card.
  - `/skills` lists the skills available.
  - `/copy` copies the last reply.
  - `/fast`, `/fast on` and `/fast off` drive fast mode.
  - `/btw` or `/side` asks a side question.

  Anything else goes to the agent as typed. Codex's `/compact` compacts its thread.

**Side questions.** `/btw <question>` asks something about the conversation without adding to it, even
mid-turn. The answer appears in a card over the pane. It comes from a fork of the session, at low
effort and with no tools. Cursor can't fork a session, so its side question starts with no context.

**Picking up again.** The empty worktree lists **Pick up where you left off**: past conversations from
all three providers, recorded in `~/.config/wtm/sessions.toml`. wtm stores no transcripts itself. On
resume, the provider supplies the history.

**Skills.** `/` lists the skills found on disk, then whatever the provider reports:

- Claude: `.claude/skills` and `.claude/commands`.
- Codex: `.agents/skills`.
- Cursor: `.cursor/skills`, `.agents/skills` and `.claude/skills`.

**Replies.**

- Tables render.
- Code blocks get a Copy button. SQL blocks also get **Run**, which opens the
  [Database console](#database-viewer).
- A file path in inline code, such as `src/app.py:42`, opens in the [Code tab](#code-tab) at that
  line.

### Delegation

Ask an agent to involve another ("have Codex review this") and it can hand work to any agent in the
same worktree through wtm's MCP tools:

- `ask_agent` opens one child as a live pane, and waits for its reply.
- `spawn_agents` runs up to 20 independent tasks in waves, four at a time unless told otherwise. Each
  task can have its own agent, model, effort and mode, and the caller gets every result back.
- `close_agents` closes the caller's finished children and leaves busy ones running.

Children don't take a tile of their own. They appear in the **Agents** rail above the panes, where
**All agents** lets you show, split or close each one. A child's approval cards are relayed into its
parent's pane, so you can answer them without going looking. Each child gets ten minutes. A
repository decides which agents can be delegated to with `[agent.<id>] enabled`.

You can hand work off too: a saved plan has **→ ‹Agent›**, which asks another agent to review it.

**When an agent runs out of usage**, a banner says so, with when the limit resets if the provider
says. It offers **Continue on ‹another agent›**, which opens a new pane seeded with a summary of the
conversation so far. The old pane stays resumable.

### wtm's MCP tools

Every session gets an MCP server called `wtm`:

| Tools | What for |
|---|---|
| `ask_agent`, `spawn_agents`, `close_agents` | Delegation, above |
| `list_sessions` | The other sessions in this worktree. Only with session awareness on |
| `browser_*` | Drive the worktree's browser panes; see [Browser panes](#browser-panes). Turned off with Settings → *Agents may use browser panes* |
| `code_read_comments`, `code_resolve_comment` | Read your [Code-tab](#code-tab) comments, and resolve them with a note you see on the comment |

The [Home agent](#home) gets a different set instead, because it has no worktree for these to act on:
`list_projects`, `list_all_sessions`, `read_session`, `message_session`, `open_session`,
`interrupt_session`, `close_sessions`, `preview_worktree` and `create_worktree`. Sessions are named by short handles such as `s2`, and
what one session said reaches Home marked as untrusted content.

**Session awareness** is a beta, off by default (Settings → General). When other sessions share the
worktree, wtm adds a short note to a session's next message saying who else is there and what state
each is in, with the first few words of each one's first prompt. Nothing else is shared, and it
never crosses worktrees.

### Usage limits

The **Usage limits** button in the title bar opens every installed agent's account side by side, with
each window's percentage used and when it resets:

- **Claude** is asked when the dialog opens: its 5-hour and weekly windows, and each model's own
  weekly limit, such as Fable's. wtm asks through the CLI's own usage request in a session that is
  sent no message, with your settings, hooks and MCP servers left out. A running session also
  updates the 5-hour and weekly figures with each reply.
- **Codex** is asked when the dialog opens. It reports its windows, plan, and any free resets.
- **Cursor** shares its plan, but not its usage.

**New ▾** shows each agent's tightest limit, and a pane's context card shows its own agent's. The
figures are held in memory, so a relaunch starts without them.

### When a session needs you

A session's status (*needs you*, *working…*, *done*, *failed*) shows in its pane's header and on its
worktree's row in the sidebar. A project with a session waiting gets a ● in the project picker, and
the dock badge counts the sessions waiting on an approval.

- **wtm in front, another worktree on screen:** a card appears, up to four at once. Clicking it takes
  you to the pane.
- **wtm in the background:** it can post a macOS notification instead. It asks the first time one
  would have been useful, and Settings → Notifications turns them on or off. Notifications need the
  signed release; see [Troubleshooting](#troubleshooting).
- **Making them stay on screen:** macOS shows a notification either as a banner, which slides away
  into Notification Center after a few seconds, or as an alert, which stays until you deal with it.
  wtm asks macOS to start new installs with alerts. To change an existing install, go to System Settings →
  Notifications → Worktree Manager and choose **Persistent**, or **Alerts** on older macOS. A Focus
  mode holds back both kinds unless Worktree Manager is on its allowed list.

### Dictation

Hold or tap the mic in the composer. The audio goes to Deepgram Nova-3, and the transcript lands in
the draft without being sent. Dictation is off by default, and turning it on asks first. It needs SoX
(`brew install sox`) and a Deepgram key, entered in Settings → Advanced and kept in the macOS Keychain.
wtm sends the recording to Deepgram itself and deletes it afterwards.

### Configuring agents for a repository

```toml
[agent.claude]
model  = "opus"
effort = "high"
mode   = "plan"            # in the provider's own spelling
fast   = false

[agent.cursor]
enabled = false            # hidden from New ▾ and from delegation
```

A new pane starts on the repository's model, effort and mode, and what you pick in the pane after
that wins. `model` goes to the CLI as written, so it can name a model the picker doesn't list; the
picker then shows it under its own id. `extra_args` appends arguments to the agent's command line,
`[agent.<id>.env]` sets environment variables for it, and `[agent.<id>.mcp.<name>]` (`command`,
`args`, `env`) adds an MCP server to that agent's sessions in this repository. The name `wtm` is
reserved. The `[defaults]` table of `~/.config/wtm/config.toml` sets any of this for every project.

**A model running on your machine.** wtm talks to the agent CLI, never to a model, so a local model
means pointing the CLI at a local server that speaks its provider's API. For Claude Code that is an
Anthropic-compatible endpoint:

```toml
[agent.claude]
model = "local-model-name"   # whatever the server calls it

[agent.claude.env]
ANTHROPIC_BASE_URL = "http://localhost:11434"   # plus any key variable your server asks for
```

The usage view keeps showing your Anthropic account, which a local server doesn't draw on. Like
`extra_args`, an `env` table puts the file through the [trust prompt](#trust-prompt).

## Worktrees

### Creating one

**+ New Worktree** at the bottom of the sidebar opens the form. With the sidebar hidden, it's **New
worktree…** in the switcher. On the left are the fields from the project's config. On the right is a
review that updates as you type:

- **An existing branch that matches**, which you can adopt instead of creating a new one.
- **What will be created**: the branch, the directory, and the base, including whether it will be
  fetched first.
- **The resolved values.**
- **The exact commands**: the `git worktree add` and setup argv, and the directory setup runs in.
- **Warnings and preflight checks.**

**Create worktree** stays disabled until preflight is clear, or until you've ticked *Do it anyway* on
the checks that can be overridden. While it runs you see each step and the setup command in a live
terminal you can type into, and *Cancel setup* stops it. If setup fails, the worktree is kept, with
*Re-run setup* and *Remove the worktree* buttons.

### Removing one

⋯ → **Remove Worktree…** shows the path and branch, then the options:

- *Also delete the branch*.
- *Force — discard uncommitted and untracked files*.

Preflight re-runs as you change them. The project's teardown steps run first, so containers are
stopped before their directory goes. If a teardown step fails, nothing is removed and you see its
output. The main worktree can't be removed.

### Details

⌘I, or ⋯ → **Details…**, shows:

- The path, with a copy button.
- The branch and HEAD.
- Staged, modified and untracked counts.
- Ahead and behind.
- Whether it's locked or stale, with git's reason.
- A **Ports** table from `[[display.port_table]]`.
- The worktree's **Environment**; see [Environment values](#environment-values).

## Registering a project

Point wtm at any git repository with **Add a repository…** in the project picker. It reads the repo
with `git worktree list --porcelain -z` and shows what's there — no config required, nothing written
to the repo.

Config is resolved in four layers, most specific winning:

| Layer | Path | Committed? | Use it for |
|---|---|---|---|
| Local | `$(git rev-parse --git-common-dir)/wtm.local.toml` | no — lives inside `.git` | machine-specific overrides, or configuring a repo you don't own |
| Repo | `<repo>/wtm.toml` | yes | the shared team convention |
| User | `~/.config/wtm/config.toml` | n/a | registered projects, preferences, and `[defaults]` for every project |
| Built-in | `defaults/wtm.default.toml` | — | a working New Worktree form with zero configuration |

Tables merge key by key; an array in a more specific layer replaces the one below it.

To try the bundled example against a repo without touching its tracked files:

```bash
just install-example REPO=~/code/myproject
```

That writes `wtm.local.toml` into the repo's git directory, which is untracked and shared by all of
that repo's worktrees. Move it to a committed `<repo>/wtm.toml` when you want to share it with the
team. Pass `CONFIG=` to install your own file instead of the bundled example — useful for a config
that describes an internal project and should not live in this repository.

## Writing `wtm.toml`

[`examples/webapp.wtm.toml`](examples/webapp.wtm.toml) is the fully-commented reference: a form with an
issue-tracker lookup, a computed slug, branch/directory naming templates, a PTY setup command, Docker
teardown steps, a per-worktree port table, agent settings, and guards for the commands that cannot be
run from a GUI. The short version:

```toml
schema_version = 1

[project]
name = "some-lib"

[[field]]                      # → a text input on the New Worktree form
key = "name"
label = "What are you working on?"
kind = "text"
required = true

[[field]]                      # → a dropdown whose options come from a shell command
key = "base"
label = "Base"
kind = "select"
default = "HEAD"
[field.options]
kind = "command"
run  = ["git", "for-each-ref", "--format=%(refname:short)", "refs/heads"]
cwd  = "repo_root"
parse = "lines"

[naming]                       # → what gets created
branch    = "{{ name | slugify }}"
directory = "{{ name | slugify | truncate(40, '') }}"
dir_base  = "repo_parent"
```

Everything else has a default. The whole schema, in one table. An unknown key is an error rather than
something silently ignored:

| Section | What it does |
|---|---|
| `[project]` | `name`, and `[project.vars]` constants for templates (`vars.<key>`) |
| `[[field]]` | The New Worktree form, in order. Kinds are `text`, `multiline`, `number`, `bool`, `select`, `multiselect`, `path`. Options are static `values`, or a `command` whose output is parsed as `lines`, `json` or `nul` |
| `[[lookup]]` | Run a command (an issue-tracker CLI, say) and map its JSON into `lookup.<id>.<key>` |
| `[[computed]]` | Values derived from the others, evaluated in order (`computed.<key>`) |
| `[naming]` | The branch and directory templates, where directories go, and an optional regex the branch must match |
| `[create]` | Which field is the base, whether to fetch it first, upstream tracking, and how a matching existing branch is offered |
| `[setup]` | The command run in the new worktree, often in a PTY, with `[[setup.args_when]]` flags |
| `[remove]` | Teardown steps (`[[remove.pre]]`), and the clean, force and delete-the-branch rules |
| `[display]` | `title`, `subtitle`, and `[[display.source]]` files (`.env` or JSON) that become `env.*`. Plus `[[display.badge]]`, `[[display.link]]` and `[[display.port_table]]` |
| `[browser]` | Where new browser panes start; see [Browser panes](#browser-panes) |
| `[database.<id>]` | Connection profiles for the [Database viewer](#database-viewer) |
| `[agent.<id>]` | Per-repository agent settings; see [above](#configuring-agents-for-a-repository) |
| `[[guards.forbid]]` | argv patterns wtm refuses to run, with the reason it shows instead |
| `[[action]]` | Accepted and validated, but nothing in the current window runs one yet |

A command is an argv array, never a shell string. Each takes `cwd`, `env`, `timeout_ms`, `pty`, `when`
and `on_failure`.

Templates are [minijinja](https://docs.rs/minijinja). They can use:

- **Tokens:** the form's field keys, plus `computed.`, `lookup.`, `vars.`, `repo.`, `worktree.`,
  `env.`, `os.` and `now.`.
- **wtm's filters:** `slugify`, `truncate`, `default_if_empty`, `re_replace`, `matches`,
  `strip_prefix`, `strip_suffix`, `after` and `before`.
- **minijinja's built-in filters.**

Each position checks the tokens it allows when the config loads. For example, naming can't use
`env.*`, because the worktree doesn't exist yet.

> **One template gotcha worth knowing.** An *undefined* token is not equal to `''`, so a
> `when = "env.FOO != ''"` guard is **true** when `FOO` is unset — the opposite of what it reads
> like. Write `when = "env.FOO | default_if_empty('') != ''"` instead.

> ### Trust prompt
>
> A `wtm.toml` or `wtm.local.toml` is not used until you approve it if it declares any of these: a
> `run` command, an agent MCP server, agent `extra_args` or `env`, or a `[database.*]` profile. The
> prompt lists every command's argv verbatim (an agent's environment and extra arguments as
> `KEY=value claude … --flag`, since the agent's own program isn't the file's to name), and each
> database as its engine and a credential-free target;
> passwords and credential-bearing URLs are never shown. The
> approval is tied to the file's exact contents, so any edit — even whitespace — asks again. Your own
> `~/.config/wtm/config.toml` is never asked about.

## Database viewer

**Database**, in the worktree bar, is attached to the selected worktree. PostgreSQL and SQLite are
supported. A profile for another engine (`mysql` parses) is listed as unavailable rather than
attempted.

**Browsing.**

- **Picking a profile.** The picker shows each profile as its label and a credential-free target, with
  badges for its environment, scope, and read-only or read/write access.
- **Connection state.** A chip says *connected*, *connecting…* or *not connected*.
- **Schema tree.** It has a filter that searches every schema. Columns show their type, primary key and
  nullability.
- **Table data.** 100 rows a page. Click a header to sort.
- **Filtering.** A **WHERE / ORDER BY** bar takes SQL fragments. Table pages always run read-only,
  whatever you type there.

**The grid.**

- **Selecting.** Click, drag, or Shift-click. The row numbers, the headers and the corner select rows,
  columns, and everything (⌘A).
- **Moving.** Arrows move, Shift extends the selection, and ⌘+arrow jumps to the edge.
- **Copying.** ⌘C copies one cell as its raw value, or several as CSV.
- **The right-click menu** has:
  - Copy as CSV, TSV, JSON or SQL INSERT.
  - Copy column names.
  - *Filter by this value*.
  - *Open value* (also ↵). It shows the whole value, with JSON pretty-printed, up to 256 KB.
  - *Send to ‹agent›*, which drafts the selection (up to 200 rows) into that agent's composer for you
    to send.
- **Finding.** ⌘F finds in the loaded rows, with an *Only matches* toggle.

**The SQL console.**

- ⌘↵ runs the selection, or the whole editor if nothing is selected. **Cancel** stops a running query.
- Results stop at 500 rows, 256 KB per cell and 16 MB in all.
- Choosing a table seeds an empty editor with a `SELECT … LIMIT 100`.

**From an agent's reply.** A fenced `sql` (or `postgres`, `sqlite`, …) block gets a **Run** button. It
switches to that worktree's console, appends the statement, and runs it **read-only**. If you aren't
connected yet, the statement waits until you press Connect.

A local service whose port and credentials vary per worktree belongs in the repo config:

```toml
[database.local]
label = "Local database"
engine = "postgres"
scope = "worktree"             # the default
environment = "local"          # the default
host = "127.0.0.1"
port = "{{ env.DB_PORT }}"
name = "{{ env.DB_NAME }}"
user = "{{ env.DB_USER }}"
password = "{{ env.DB_PASSWORD }}"
```

`env.*` comes from the worktree's existing `[[display.source]]` files, so selecting another
worktree resolves a different port and opens a different session. A file-backed repository is the
same shape with `engine = "sqlite"` and `path = "var/app.sqlite3"`; relative paths resolve inside
that worktree.

Shared TEST/STAGING/PROD connections use `scope = "project"`. Their tabs and live session persist
while you move among that project's worktrees, but never into another project. Put machine-specific
definitions in the untracked, git-common `wtm.local.toml`; project-scoped profiles deliberately
cannot use `env.*`, because that would make one allegedly shared session depend on whichever
worktree happened to be selected when it connected.

```toml
[database.production]
label = "Production (read only)"
engine = "postgres"
scope = "project"
environment = "production"
access = "read_only"
url = "postgres://readonly-user:password@db.example.invalid/app"
tls = "require"
```

Credentials are rendered and retained in Rust, never listed over IPC or logged. Read-only profiles
are also enforced by the driver. A read/write production console has an additional per-session UI
lock; table browsing remains available while it is locked.

## Code tab

**Code**, beside Sessions and Database in the worktree bar, is a read-only view of the selected
worktree for reviewing what an agent did — without leaving wtm to find the lines and paste them back.

- **Project tree.** What git lists, with ignored folders like `node_modules` shown dimmed and
  loaded only when opened. **Changes** switches to just the files this branch changed since its
  base (or only what is uncommitted), each marked A, M, D, R or U. The tree re-reads when the tab is
  shown, on window focus, and when an agent in the worktree finishes a turn.
- **Viewer.** Syntax highlighting for Python, JavaScript/TypeScript, Vue, Svelte, HTML, CSS/SCSS,
  JSON, Markdown, Rust, Go, YAML, SQL, XML, TOML, shell and more; ⌘F finds in the file. A changed
  file has a gutter of change markers — click one to see what the lines were.
- **⇧⌘F Find in Files** — match case, words and regex (⌥C, ⌥W, ⌥X), a file mask such as
  `*.py, !*.min.js`, and a preview of each hit. Ignored files are left out unless you ask.
- **⇧⌘O / ⌘O / ⌥⌘O** — go to a file, a class, or any symbol; Tab switches between them. ⌘-click or
  ⌘B on a name goes to its definition. Definitions come from the code's shape, not a language
  server, so there is nothing to install and two things with one name are both offered.
- **Comments.** Select lines and choose **Comment** (or ⌥⌘C) to leave a note on them, or **Ask
  ‹agent›** to put them straight into that agent's message box. The Review drawer's **Send** drafts
  every new comment, quoted, into the agent you last used here — the chevron picks another — and
  never sends anything until you press Enter there. An agent can also read the comments itself with
  `code_read_comments` and resolve them with a note you see on the comment. Comments are kept until
  wtm quits.
- **From an agent's reply**, a file path in inline code — `src/app.py:42` — opens in the Code tab at
  that line, when the file exists in the worktree.

## Browser panes

A browser pane is a real web page — the platform's WebKit, the same engine the app itself runs on —
tiled beside your shells and agent sessions in a worktree. Open one from **New ▾ → Browser** in the
worktree bar, the empty surface's **Browser** button, or a browser pane's Split control. New panes
open the first visible, openable HTTP(S) `[[display.link]]` in repository order, with templates
resolved for that worktree.
Without an available link, the pane starts empty; type an address (a bare
`localhost:5173` gets `http://`, anything else `https://`), or pick one of the worktree's
`[[display.link]]` URLs from the empty state. The pane has Back, Forward, Reload, an address bar
(⌘L), and opens the page in your real browser on request. It follows its tile when you split,
drag or resize, disappears while a dialog is up or another worktree is selected, and comes back
after a relaunch at the address it was on.

A project can override where new panes start with `[browser].home`:

```toml
[browser]
home = "http://localhost:{{ env.WEB_PORT }}"   # rendered per worktree, like a display link
```

**Agents can use it.** Every agent session in the worktree gets `mcp__wtm__browser_*` tools:
`browser_open`, `browser_list`, `browser_navigate`, `browser_history`, `browser_snapshot` (the page
as an outline with element refs — the primary way an agent reads a page), `browser_click`,
`browser_type`, `browser_fill_form`, `browser_select_option`, `browser_press_key`, `browser_hover`,
`browser_scroll`, `browser_wait_for`, `browser_screenshot`, `browser_get_content`,
`browser_console`, `browser_evaluate`, `browser_read_comments`, `browser_resolve_comment` and
`browser_close`. Ask a session to "open the dev server and check the signup form" and a pane
appears in the worktree; the element it acts on flashes, and the pane's header says who is driving.
Each pane has an **Agent access: on/off** toggle to allow or block that, Settings has a global
switch, and an agent can close only the panes it opened. Everything an agent reads from a page
arrives wrapped as untrusted web content — see ARCHITECTURE §6c for the whole trust story.

**Comments.** Turn on **Comment** in the pane's toolbar (⇧⌘C), hover to see what you would pick,
click an element and say what should change — "make this a blue button". A numbered pin stays on
the element, the comments list opens beside the page, and **Send N to ‹agent›** drafts them into the
focused agent's composer with the element, its text and a CSS selector, for you to read and send.
Agents can also fetch them with `browser_read_comments` and mark them resolved.

**What has been verified, and how.** On macOS, driven from the app's own log rather than by a
hand on the mouse: a pane opened through the store's ordinary path is placed exactly over its tile
and moves with the layout; an agent-opened pane is adopted into the worktree; a pane is restored
after a relaunch and reloads its address; a remote page's attempt to `invoke` the app is refused
and its attempt to navigate onto `tauri://localhost` goes nowhere; and the tool sequence
open → snapshot → click (which navigated) → screenshot → console → evaluate → content → comments →
history → close ran against a live page and returned what it should. Not yet exercised by a person:
comment mode's in-page pins and popover, the comments panel, the hide-behind-a-dialog placeholder,
and the keyboard chords — all wired, none clicked.

**Limits worth knowing.** Clicks and keys an agent sends are synthesized DOM events, not OS input:
links, buttons, checkboxes, and framework handlers all work, but a native `<select>` popup, a file
chooser, or a `window.open` that needs a real user gesture will not (use `browser_select_option`
for selects). Downloads and popups are handed to your default browser and the same pane
respectively. Four browser panes per worktree, eight in all — each is a WebContent process.

## Panes in their own windows

Every pane you can drag around the tiling — a shell, an agent chat, a browser — can also leave it.
Press the **Pop out** button in the pane's header (the two overlapping windows, beside Split) and
the pane moves into a window of its own, opened over the tile it came from, which you can put on
another display. The session does not restart: a shell keeps its scrollback and whatever it is
running, an agent keeps its transcript, its queue and the draft in its composer, and a browser keeps
the very page it had — history, scroll position, a half-filled form — because it is the same web
view, moved.

To put it back, press **Put back** in the window's title bar, or just close the window — the traffic
light and ⌘W do the same. It returns to exactly where it was if nothing else in the tiling moved
while it was away, and beside the pane it was next to if something did. Closing the window never
ends the session; the pane's own **Close** does that, from either window.

While a pane is out, the main window lists it in a strip above the worktree's panes: press its name
to bring its window forward, or the arrow to put it back. Its status dot, the sidebar and the dock
badge keep counting it, and a notification about it is held back while its window is the one in
front. A browser in its own window can still send its comments to an agent in the main window.

**What has been verified, and how.** `just check`: the Rust registry, the browser placement rule
(a hide from a window that no longer holds a browser is ignored; a closing window can never take one
back), the terminal replay ring and its lifecycle, and a capability test proving the pane-window
glob can never match a browser's webview. The new controls were rendered against the real
stylesheet in headless Chrome. In the running app, an agent pane has been popped out and put back
— which is how a resumed conversation's pop-out was found to be blank: its history had never
reached the replay the new window paints from, now fixed. Shell and browser pop-outs, and closing
the window from its traffic light, have not been clicked through yet.

**Limits worth knowing.** Pane windows do not survive a relaunch — the panes come back tiled where
they were. A shell's replay is its last mebibyte of output, so a very long history comes back
trimmed, and a full-screen program redraws only once the new window's size reaches it. Closing the
window from its traffic light carries the draft as it was a quarter of a second earlier; **Put back**
carries it exactly. While a pane is out the main window holds a second copy of an agent's
transcript.

## Open in …

A split button in the worktree bar. The left half hands the worktree's directory to your
preferred tool; the right half is a menu of everything wtm knows about. Picking one launches
it **and** makes it the default, stored as `ui.opener` in `~/.config/wtm/config.toml`.

Supported: Claude Code (in a terminal, or handed to Claude Desktop), VS Code, Cursor,
Windsurf, Zed, PyCharm, IntelliJ IDEA, WebStorm, Sublime Text, Fork, a terminal, and Finder.

Tools you do not have are **listed but disabled**, with the reason — usually *"no `code` on
wtm's PATH"*. That is deliberate: it doubles as a diagnosis of this app's most likely failure,
a GUI-launched process that cannot see your shell's `PATH` (see
[Troubleshooting](#troubleshooting)). On macOS a tool is found either by its shell command or
by its `.app` bundle, so VS Code works whether or not you ever ran *Shell Command: Install
'code' command in PATH*.

Worth knowing:

- **The two Claude entries go to different places, and are detected separately.** *Claude
  Session* uses the `claude-cli://open?cwd=…` deep link the `claude` CLI registers, and that
  handler starts a session in a **terminal emulator** — iTerm2 if you have it, otherwise
  Ghostty, Kitty, Alacritty, WezTerm or Terminal.app, in that order. It needs `claude` on
  wtm's `PATH`. *Claude Desktop* uses `claude://code/new?folder=…`, which the desktop app
  itself registers, and lands in the app with no terminal involved; it needs the app
  installed, not the CLI. Either can be offered without the other.
- **Claude Desktop asks to trust the folder the first time.** That prompt is the app's own,
  once per directory, and wtm neither suppresses nor pre-answers it.
- **Fork opens the worktree, not the repository it was cut from.** Its CLI takes a *command*
  rather than a path (`fork open`), so wtm runs it with the worktree as the working
  directory — same mechanism the terminal opener uses. Verified against a real linked
  worktree, where `.git` is a file rather than a directory: Fork resolves it correctly and
  lands on that worktree's branch.
- **Nothing here is a project config concern.** Openers are built in and identical in every
  repository, so they need no `wtm.toml` entry and trigger no trust prompt. The catalogue is
  compiled in, so adding a tool is currently a one-entry code change in
  `src-tauri/src/openers.rs` rather than something you can do from a config file.

## Environment values

A worktree's `.env` often holds real credentials, and this app displays that file. How it is
handled:

- **The webview cannot reach the network.** The CSP permits only `self` and IPC; the update
  check, database protocols and opt-in dictation live behind narrow Rust commands. There is no
  telemetry.
- **Nothing is logged.** No log line carries an environment value.
- **No value is sent to the window.** Not "no secret" — *no value*. The listing carries key
  names only; the Environment section of **Details** shows `••••••••` for every row with a
  per-key **reveal**, which fetches that one value on demand and reads it fresh from disk.
  Closing the dialog masks everything again. A screenshot or a screen-share cannot leak what
  was never sent.

The keys come from the first `[[display.source]]`. Badges, links and the port table are
different: they render values into the window by design, because showing them is their job.

There is deliberately no attempt to work out which keys are secrets. An earlier version
classified them by key name, by whether the value looked like `scheme://user:pass@host`, and
by whether a value matched another key's secret. It worked, and it was still the wrong shape:
guessing fails in two directions — under-match and a credential is published, over-match and a
port number needs a click — and every project's `.env` gets a vote on which way. The type the
listing uses can no longer hold a value at all, so this is a property of the design rather
than a policy that has to be kept correct.

`cargo test -p wtm-app --test env_masking` proves it, against a repo whose `.env` is nothing
but credentials. It runs as part of `just check` — it no longer needs a real checkout, because
the guarantee no longer depends on the data.

## Settings

**⌘, or the gear in the title bar.** Changes apply as you make them; there is no OK button. The
exceptions are the `PATH` override, which has a Save button and takes effect after a restart, and
the transcription key.

| Section | What is in it |
| --- | --- |
| Appearance | Colour palette, and light / dark / follow the system |
| General | Whether ⌘↵ or ↵ sends a message. Which tool **Open in …** defaults to. The automatic update check. **Agent coordination**: whether agents may use browser panes, and session awareness (beta). **Dictation**: on or off, hold-to-talk or tap, and words to listen for |
| Notifications | Whether wtm notifies you when a session needs attention, and what macOS says about it |
| Advanced | The `PATH` override, and the Deepgram key, kept in the macOS Keychain. Plus read-only diagnostics: the PATH wtm actually resolved and where it came from, the config directory, and which of the common tools it can find |

Advanced is where to look first when a project's commands work in your terminal and not in
wtm. See [the PATH problem](#the-path-problem).

Everything lives in `~/.config/wtm/`. `config.toml` holds preferences, registered projects and
their sidebar layouts. `trust.toml` holds your trust decisions, `sessions.toml` the conversations
you can resume, `plans/` the saved plans, and `wtm.log` the log. Two dictation keys have no control
in the dialog: `ui.dictate_language` (default `en`) and `ui.dictate_max_seconds` (default 120).

### Palettes

Nine ship with the app: **Pine** (the default), **Clay** (the terracotta wtm wore before
v0.4), **Slate** (near-neutral, for no colour at all), **Harbor**, **Plum** and **Rose**,
and three with quieter accents: **Paper**, cream and warm grey with an ink-blue accent;
**Fog**, a cool blue-grey; and **Dusk**, lavender. Pick one of those three if the other six
feel loud over a long session.

Every palette works in both light and dark. Dark mode sits on charcoal (about `#232323`)
rather than near-black.

If none of them suit, declare your own. It appears in the picker beside the built-in nine:

```toml
[ui.palettes.nord]
name   = "Nord"          # optional; the table key is used when absent
hue    = 250             # oklch hue angle, 0–360
chroma = 0.9             # how strongly the greys are tinted. 1 is the reference, 0 is flat
brand  = ["#88c0d0", "#81a1c1", "#5e81ac", "#4c688f"]
```

`hue` and `chroma` are all the neutral ramp needs — every surface, border and text colour in
the app is derived from them in oklch, at lightness values fixed by the stylesheet. That is
what keeps a hand-written palette as readable as the built-in ones: you choose the hue, and
the contrast ratios are not yours to get wrong.

`brand` is the accent, from lightest to darkest. **Dark mode uses the first two and light
mode the last two**, so pick the first pair to read against charcoal and the last pair
against near-white — not to look good as a row of four.

A palette that cannot be used is still listed, greyed out, with the reason on hover. Bad hex,
a hue outside 0–360, or anything other than exactly four `#rrggbb` colours will do it. The
rest of your config still loads.

## Keyboard shortcuts

| Where | Keys | What they do |
|---|---|---|
| Anywhere | ⌘, | Settings |
| | ⌃⌘S | Show or hide the sidebar |
| | ⇧⌘H | Home, and back |
| | ⌘R | Refresh the worktree list |
| | ⌘I | Details (except while focus is in a pane) |
| | ⌘F | Filter the worktrees. The Database and Code views use it to find in themselves instead |
| | ⌘J | Focus a shell, opening one if there is none; press again to cycle through them |
| | ⇧⌘O / ⌘O / ⌥⌘O | Go to File / Class / Symbol, switching to the Code tab |
| | ⇧⌘F | Find in Files, switching to the Code tab |
| Sidebar | ↑ ↓ Home End | Move between worktrees and groups |
| | ← → · F2 | Fold or unfold a group · rename it |
| | ⌥↑ ⌥↓ | Move the row or group |
| Composer | ⌘↵ | Send, or queue while a turn runs |
| | ⇧⌘↵ | Steer the message into the running turn |
| | ↵ / ⇧↵ | Send / new line, when Settings says ↵ sends |
| Browser pane | ⌘L · ⌘[ ⌘] · ⌘R · ⇧⌘C | Address · back, forward · reload · comment mode |
| Code tab | ⌘F · ⌘B or ⌘-click · ⌥⌘C | Find in the file · go to definition · comment on the selection |
| Database | ⌘F · ⌘↵ · ⌘C · ⌘A · ↵ | Find in the grid · run · copy · select all · open the value |
| Pane window | ⌘W | Put the pane back |

---

Everything from here on is about building it yourself.

## Prerequisites

| Tool | Version | Install |
|---|---|---|
| macOS | 13+, Apple silicon | — |
| Xcode Command Line Tools | any | `xcode-select --install` |
| Rust | pinned to 1.97.1 by `rust-toolchain.toml` | see below — **rustup, not a package manager** |
| Node | 20.19+ / 22.12+ | `brew install node` |
| bun | 1.x | `curl -fsSL https://bun.sh/install \| bash` |
| `claude`, `codex`, `cursor-agent` *(optional)* | any | whichever agents you want sessions with |
| `just` *(optional)* | 1.50+ | only needed by projects whose config calls it |
| `acli` *(optional)* | any | Atlassian CLI — only for Jira-backed form fields |
| `gh`, `docker` *(optional)* | any | only if a project's config uses them |
| `sox` *(optional)* | any | dictation only — `brew install sox` |
| `curl` | any | the update check and dictation — already present on macOS |

**Full Xcode is not required.** Command Line Tools is enough for desktop Tauri.

Install Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
```

Then restart your shell, or `. "$HOME/.cargo/env"`.

> **Do not install Rust from a package manager** — not `brew install rust`, not `apt install rustc`.
> Those builds ignore `rust-toolchain.toml`, cannot add build targets, and upgrade themselves during
> unrelated system upgrades — which invalidates `target/` and costs you a full rebuild. `just doctor`
> warns if a packaged rust is shadowing the rustup shims.

## Setup

```bash
just setup
```

Idempotent, safe to re-run. It pins the toolchain, adds `rustfmt`/`clippy`/the wasm target, installs
`cargo-nextest`/`cargo-deny`/`bacon`, installs frontend dependencies, and points
`core.hooksPath` at `.githooks`.

Then check your machine:

```bash
just doctor
```

`doctor` reports each tool twice over: whether it's on your **current** PATH, and whether it's on your
**login-shell** PATH. That second check matters — see [the PATH note](#the-path-problem) below.

## First run

```bash
just dev
```

> ⏱️ **The first build compiles ~800 crates: 3–6 minutes on an M-series Mac, and `target/` grows to
> 3–6 GB.** Later runs are seconds. Frontend edits hot-reload; Rust edits trigger a partial rebuild
> and a window restart.

On first launch wtm creates `~/.config/wtm/config.toml`. Open it with `just config`.

## Dev workflow

```bash
just dev        # run the app with hot reload
just web        # the frontend alone, no Rust — for CSS and theme work
just watch      # bacon: check → clippy → test, in a second pane
just fmt        # format Rust + web
just check      # everything CI runs — do this before pushing
just audit      # licenses + RUSTSEC advisories
just doctor     # what's installed, and the PATH the app will actually use
just logs       # tail ~/.config/wtm/wtm.log
just config     # open ~/.config/wtm/config.toml in $EDITOR
just icon       # redraw src-tauri/icons from assets/brand/wtm-icon.svg
just clean      # cargo clean, plus dist, node_modules and src-tauri/gen
just release 3.1.1  # test, tag, sign, publish, and update the Homebrew tap
```

`just release <version>` starts only from a clean, up-to-date `main`. It updates every version
field, runs `just check` and `just audit`, pushes the release commit, and waits for CI before
creating the tag. The tag starts the Release workflow, which builds, signs, notarizes, and checks
the app before drafting the release. The command then downloads the artifact and checks it again:

- the checksum, and exactly the two expected assets;
- the version, the minimum OS, and an arm64 binary;
- the signature and Team ID, the stapled notarization ticket, and Gatekeeper's verdict.

Only then does it publish the draft and push the verified checksum to
`TakumiHendricksDev/homebrew-tap`. A stopped command can be run again with the same version to
resume completed stages safely.

Commits are signed through the 1Password SSH agent (`commit.gpgsign=true` globally), so 1Password must
be running and unlocked or the commit will hang waiting on Touch ID.

## Build & install

```bash
just build         # the .app  (~35 s warm, ~70 s cold)
just run           # build, then launch it
just install-app   # build, then install it to /Applications
just build-dmg     # ⚠️ prompts for Finder Automation permission the first time
```

`build` produces `target/release/bundle/macos/Worktree Manager.app`, and `install-app` copies it
into `/Applications` and re-registers it with LaunchServices.

Warm is ~35s no matter how little you changed, because `tauri build` regenerates the context
`wtm-app` compiles against on every run, so that one crate always recompiles. That number used to be
minutes; ARCHITECTURE.md § Build performance has the measurements and what moved them.

CI builds the same bundle on every push, so a break in bundling is caught without anyone
building it by hand.

**Releases are signed and notarized; a local build is not.** The Release workflow signs with a
Developer ID certificate from the repository's secrets, notarizes with an App Store Connect API key,
and refuses to publish an app that didn't come out signed. `release.yml` names each secret and says
why it's there.

- A local build is signed ad-hoc by the linker. That's enough to run it on the Mac that built it,
  but two things key on the signature and so behave differently from a release: the microphone
  grant resets on every rebuild, and macOS won't deliver notifications at all. Both are in
  [Troubleshooting](#troubleshooting).
- To sign a local build with a Developer ID you hold, export `APPLE_SIGNING_IDENTITY` (the name
  `security find-identity -v -p codesigning` prints) before `just build`. Add `APPLE_API_ISSUER`,
  `APPLE_API_KEY` and `APPLE_API_KEY_PATH` and Tauri notarizes it too.
- Universal binary: `rustup target add x86_64-apple-darwin && just build-universal`. Roughly doubles
  build time.

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| App works under `just dev`, then "program not found" once installed | <a name="the-path-problem"></a>A `.app` launched from Finder inherits `PATH=/usr/bin:/bin:/usr/sbin:/sbin`. `just`, `acli`, `docker`, `bun` all live in `/opt/homebrew/bin`. | wtm probes your login shell's PATH at startup and uses it for every spawn. If a tool is still missing, set `exec.path` in `~/.config/wtm/config.toml`. `just doctor` flags any tool that isn't on the login PATH. |
| An agent is missing from **New ▾** | Its CLI isn't on wtm's PATH, or the repository set `[agent.<id>] enabled = false` | An empty worktree's launcher lists it greyed out, with the reason on hover. Install the CLI, or see [the PATH problem](#the-path-problem). |
| `just: command not found` | `/opt/homebrew/bin` missing from a non-login shell (common in editor terminals and launchd) | `brew install just`, and add `/opt/homebrew/bin` to PATH in `.zprofile` |
| `error: rustup could not choose a version of cargo` | rustup absent, or a Homebrew `rust` shadowing the shims | Install via rustup; `which -a cargo` must list `~/.cargo/bin/cargo` first |
| First `just dev` seems hung | It isn't — ~800 crates, with long silences on `tao`, `wry`, `objc2-app-kit` | Wait 3–6 minutes. `just watch` in another pane shows progress on our own crates. |
| Jira fields come back empty, form still works | `acli` not authenticated or offline. Lookups are `on_error = "warn"`, so fallbacks apply and creation is never blocked. | `acli jira auth login --web` |
| `svelte-check` errors about the TypeScript version | `typescript@7` is `latest` but `svelte-check` peers `^5 \|\| ^6` | TypeScript is pinned to `~6.0.3` in `package.json` — don't bump it to `latest` |
| `vite build` fails with "Failed to load `transformWithEsbuild`" | Vite 8 uses Rolldown/Oxc; the esbuild minifier is now a separate install | `vite.config.ts` sets `minify: 'oxc'`. Don't change it back to `'esbuild'`. |
| The microphone prompt comes back after every `just build` | A local build is signed ad-hoc, and macOS keys the microphone grant to a signature that changes on each rebuild. A release's Developer ID signature stays the same across versions, so its grant survives updates. | Expected for builds you make yourself. Approve it again, or sign local builds with `APPLE_SIGNING_IDENTITY` ([Build & install](#build--install)). |
| Turning on notifications says macOS won't deliver them | You're running a local build. macOS's notification service turns away an app whose code signature doesn't name its bundle, and an ad-hoc build is signed under its binary's name (`wtm-…`) rather than `dev.takumihendricks.wtm`. It isn't listed in System Settings → Notifications for the same reason. | Install the signed release from the tap, or sign local builds with `APPLE_SIGNING_IDENTITY`. Meanwhile in-app cards, the sidebar's dots and the dock badge still say when a session needs you. |
| Dictation says it needs `rec` | SoX is not on the resolved PATH. `rec` is SoX's recording front-end and ships with it. | `brew install sox`, then reopen Settings so the check re-runs. See [the PATH problem](#the-path-problem) if it is installed and still not found. |
| Dictation inserts nothing and says nothing was recorded | The microphone is muted or another application holds it. wtm records perfect silence happily and the service accepts it. | Check the input device in System Settings → Sound, and that no other app is recording. |
| The transcription key is rejected | The key is for a different Deepgram project, or was pasted with surrounding whitespace | Re-paste it in Settings → Advanced. wtm trims it, but a key copied with a line break from a terminal can pick up more than whitespace. |
| The app window opens behind another app | A bare binary launched from a shell does not activate | Use `just run`, or `open "…/Worktree Manager.app"` — a bundled app activates properly |
| `open` fails with `error -600` | Rebuilding over the same bundle path leaves LaunchServices holding a stale record | `just run` re-registers the bundle first. By hand: `lsregister -f "…/Worktree Manager.app"` |
| `@tauri-apps/cli` "cli-darwin-arm64 not found" | bun didn't resolve the platform-specific optional dependency | `rm -rf node_modules bun.lock && bun install` |
| Setup command hangs forever with no output | The project's command is prompting on stdin, and a `confirm()`-style helper can loop forever on EOF rather than giving up. | Every captured command has a mandatory timeout; PTY commands are interactive — answer in the setup terminal on the New Worktree screen, or press *Cancel setup*. Add the command to `[[guards.forbid]]` so it can't be run again. |
| Worktree list is missing a worktree you just deleted by hand | git keeps stale admin entries until pruned | Refresh (⌘R); wtm prunes before it lists. Or `git worktree prune`. |
| The app quit, or something failed with no visible reason | A `.app` launched from Finder has no stderr anyone can read | `just logs` tails `~/.config/wtm/wtm.log`, which every run appends to — including panics. `WTM_LOG=debug` (`RUST_LOG` grammar) turns up the detail. |
| A checkbox in New Worktree seems to have no effect on what runs | Nothing, now — but this was a real bug | Confirm on the review screen, which shows the exact setup argv wtm will run. If a `[[setup.args_when]]` flag appears when its box is unticked, that's a bug worth reporting. |

## Logs

Every run appends to `~/.config/wtm/wtm.log`, and also writes to stderr when you're running
`just dev`. This exists because a bundled macOS app has nowhere else to put a diagnostic: launched
from Finder, its stderr goes nowhere.

```bash
just logs
```

Panics are logged too, and a panic inside a command surfaces in the UI as an error rather than
killing the app — `[profile.release]` deliberately does not set `panic = "abort"`, and a test
enforces that.

## Dependencies

```bash
just audit
```

Runs `cargo deny check` over the Rust tree and `bun audit` over the frontend: RUSTSEC advisories,
license allow-list, duplicate versions, and source registries. Kept out of `just check` because it
refreshes the advisory database over the network, and a check that fails on a train is a check people
stop running.

Two things the config decides deliberately, both written down in [`deny.toml`](deny.toml):

- **Vulnerabilities fail. Always.** cargo-deny removed the option to downgrade them, which is the
  right call and what this gate relies on.
- **`unmaintained` is scoped to crates this workspace chose.** Seventeen unmaintained advisories
  come through Tauri: `unic-*` via `urlpattern`, whose advisory says outright that no safe upgrade
  exists. Denying those would mean a permanent ignore list by advisory ID, which is where a real
  advisory goes to hide. If *we* add an unmaintained crate, it still fails.

  The count used to be seventeen, ten of them gtk-rs crates Tauri pulls on Linux. Those are gone
  now — not because the dependency tree changed but because `deny.toml` names `[targets]`, so
  cargo-deny evaluates the two Apple targets this app is built for instead of the union of every
  platform in the lockfile. A lockfile is a union; a shipped binary is not.

One version is pinned rather than current: **TypeScript is held at `~6.0.3`** because
`svelte-check@4.7.4` peers `^5 || ^6`. `latest` is 7.x and would break the type gate.

Attack surface is deliberately small — worth knowing when judging the dependency list. No HTTP
client crate is in the tree, no `fetch`/XHR/WebSocket appears in the frontend, and the webview CSP
admits no remote origin. wtm's own requests go through `curl` or a database driver, and there are
three kinds:

- the update check to `api.github.com`, which is automatic, carries nothing about you, and can be
  turned off;
- dictation audio to `api.deepgram.com`, which is opt-in;
- the database connections you configure and press Connect for.

`src-tauri/tests/network_boundary.rs` pins the two fixed hosts and the absence of an HTTP client.
Pressing *Update and restart* hands the download to Homebrew. Beyond that, what wtm runs is git, the
agent CLIs you installed (which talk to their own providers), and the commands your project config
declares.

## Architecture

Design decisions, the crate layout, the ports, and why the toolchain is pinned the way it is live in
[ARCHITECTURE.md](ARCHITECTURE.md).
