/**
 * Every session in every worktree: shells and agent chats, in one list.
 *
 * # Why one store rather than two
 *
 * `terminals.svelte.ts` solved the hard problems first — a pane outlives the view, Rust owns the
 * session, panes stay mounted and inactive ones get `display: none`, the cap refuses rather than
 * evicting. An agent chat needs every one of those. Two stores would mean two caps, two focus
 * mechanisms, two reconcile passes and two answers to "which pane is active", and the duplication
 * grows with every feature. So a shell is a `kind`, not a separate system.
 *
 * The three rules `terminals` stated still hold verbatim, and they are the reason this works:
 *
 * 1. **A pane outlives the view.** Nothing in the UI removes a pane — not switching worktrees, not
 *    switching projects. Only closing it does.
 * 2. **Rust owns the session.** This side holds an id. It never decides a session ended; the
 *    `pty:exit` and `agent:exit` events do.
 * 3. **No DOM.** Focus is not state. What is here is the record that somebody asked for it.
 */

import { listen, type UnlistenFn } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import {
  errorMessage,
  type AgentAttachment,
  type AgentEvent,
  type AgentEventEnvelope,
  type AgentExit,
  type AgentOption,
  type AgentReady,
  type AgentSkill,
  type AgentUsage,
  type ApprovalAnswer,
  type ApprovalRequest,
  type Capability,
  type PtyExit,
  type BackgroundTask,
  type Brief,
  type Resumable,
  type SpawnedSession,
} from '../ipc/types';
import { statusOf, worse, type PaneStatus } from '../status';
import { transferPrompt } from '../transfer';
import { attention, type Announceable, type Announcement } from './attention.svelte';
import {
  insert,
  move,
  panesOf,
  replacePane,
  remove,
  resize,
  type Layout,
  type Placement,
  type Target,
} from './layout.svelte';

/**
 * How many events one transcript keeps.
 *
 * A bound is unavoidable: a long session with a chatty tool streams tens of thousands of deltas, and
 * an unbounded array inside a `$state` proxy is a leak with a rendering cost attached. Overflow drops
 * the oldest, which degrades to "the top of a very long transcript is gone" rather than to an
 * unresponsive window.
 */
const MAX_EVENTS = 20_000;

/** Approximate UTF-16 bytes retained by one pane's normalized event log. */
const MAX_EVENT_BYTES = 32 * 1024 * 1024;

function eventBytes(event: AgentEvent): number {
  // Browser strings are UTF-16. This deliberately over-counts ASCII rather than allocating a
  // second `Uint8Array` for every streamed token merely to measure its UTF-8 representation.
  return JSON.stringify(event).length * 2;
}

/**
 * How many panes stay alive at once, per worktree and in total.
 *
 * `ARCHITECTURE.md` §3 sizes the pty design for "a handful of terminals" — one OS thread each in
 * Rust and one event subscription each here — and an agent CLI is heavier than a shell, not lighter.
 * The cap **refuses** rather than evicting, which was already the right answer for a shell that
 * might be running a dev server and is more so for a session mid-turn.
 */
export const MAX_PANES_PER_WORKTREE = 4;
export const MAX_PANES = 8;

/**
 * What a refused pane says.
 *
 * A constant for two reasons: `clearCapacity` compares against it to tell this banner from any
 * other error the store reports, and the surface's empty state says the same thing, which is one
 * sentence too many to keep in two files.
 */
export const AT_CAPACITY =
  'As many sessions are open as wtm keeps alive. Close one to start another — it refuses rather than ending a session that may be mid-turn.';

/**
 * What a refused *tile* says.
 *
 * Distinct from [`AT_CAPACITY`]: that one is about processes, and splitting a delegated child
 * does not start one. Reusing the process sentence here would tell the user they are about to
 * kill a session when they are only out of room on screen.
 */
export const AT_TILE_CAP =
  'This worktree already has as many panes as fit on one screen. Close or merge one to split another.';

/** What the shell spawns at, corrected as soon as the pane has measured itself. */
const SPAWN_ROWS = 24;
const SPAWN_COLS = 100;

/** How to spell the shortcut in a tooltip. Read once, pre-paint, like `terminals` did. */
export const SHELL_SHORTCUT =
  document.documentElement.dataset.platform === 'linux' ? 'Ctrl-J' : '⌘J';
export const INSPECTOR_SHORTCUT =
  document.documentElement.dataset.platform === 'linux' ? 'Ctrl-I' : '⌘I';

/**
 * How long a turn waits for its pane's session id. 100 tries, 50 ms apart — five seconds.
 *
 * Long enough for a CLI to come up on a cold cache, short enough that a spawn which failed does not
 * leave a message looking like it is still going out.
 */
const SESSION_WAIT_TRIES = 100;
const SESSION_WAIT_STEP_MS = 50;

/**
 * How many events are held for a session no pane has claimed yet.
 *
 * Small on purpose. This buffers the gap between a session being spawned and its id crossing IPC —
 * a handshake, not a conversation — so anything past a handful means the id is never coming and the
 * events belong to a session that failed to register. Holding thousands of those would be a leak
 * dressed up as robustness.
 */
const MAX_EARLY_EVENTS = 64;

export type SessionKind = { kind: 'shell' } | { kind: 'agent'; provider: string };

/**
 * Store `next` under `key`, or `null` when that would change nothing.
 *
 * The contract `workspace.merge` states, for the same reason that file gives: a freshly built object
 * with identical contents is a **new identity**, so assigning one re-runs every effect that reads
 * the map.
 *
 * Here that mattered more than it does there. The three refresh methods below are called from an
 * effect, so an assignment that always signals is not a wasted render — it is a loop, and the
 * `background` arm of it spawned a `claude` per iteration. Moving the read after the `await` is
 * what breaks the cycle; this is the second layer, and the one that still holds if a later edit
 * puts a read back in front of it. Both were measured against this project's Svelte: read-first
 * runs unbounded, either fix alone settles in one or two passes. (`untrack` at the call site was
 * the obvious third layer and does nothing at all — a pre-`await` read stays tracked inside it.)
 *
 * Deep equality via JSON, like `merge`'s and for the same reason: these lists are plain serialized
 * data straight off the wire, so it is exactly the comparison that matters and it needs no
 * per-field maintenance.
 */
function patch<T>(
  map: Record<string, T[]>,
  key: string,
  next: T[],
): Record<string, T[]> | null {
  const current = map[key];
  if (current !== undefined && JSON.stringify(current) === JSON.stringify(next)) {
    return null;
  }
  return { ...map, [key]: next };
}

/** An approval a session is blocked on. */
export interface PendingApproval {
  id: string;
  request: ApprovalRequest;
}

export interface Pane {
  /**
   * This window's own id for the pane, stable from the moment it is created.
   *
   * Distinct from `session`, which is null until Rust answers, and from the worktree. The layout
   * tree references panes by this, so a tree entry can exist before a session does.
   */
  id: string;
  projectId: string;
  worktreeId: string;
  kind: SessionKind;
  /** Backend session id. Null between asking and being told. */
  session: string | null;
  /** Agent transcript. Empty for a shell, whose transcript lives in its xterm instance. */
  events: AgentEvent[];
  /** Approximate retained size, maintained with `events` so the hot path never rescans the log. */
  eventBytes: number;
  approvals: PendingApproval[];
  /** Most recent provider-reported context totals, maintained outside the transcript log. */
  usage: AgentUsage | null;
  /**
   * True between `turn_started` and `turn_finished`.
   *
   * Maintained in `record` alongside `approvals`, and for the same reason that one gives: this was a
   * `$derived` inside `SessionPane` that scanned `events` **backwards** for the nearest of the two,
   * which is O(events) per read against a log bounded at 20 000. One reader could afford that; the
   * sidebar needs the same answer for every pane on every render, and cannot.
   *
   * A plain boolean rather than a depth counter, because the scan it replaces was already
   * "nearest wins" — an assignment reproduces it exactly, where a counter would additionally have to
   * survive an unpaired `turn_finished`.
   */
  working: boolean;
  /** The most recent submitted turn completed, maintained beside `working` for child status UI. */
  lastTurnFinished: boolean;
  /**
   * A turn finished, or a session failed, while this pane's worktree was not the selected one.
   *
   * The blue dot, and it is **unread rather than recent**: it is cleared by looking (see `markSeen`),
   * never by elapsed time. Deliberately, and not only because polling and timers are banned here —
   * "did anything happen while I was away" is the question a person actually has, and it is not the
   * same question as "did it happen in the last five minutes".
   */
  unseen: boolean;
  ready: boolean;
  ended: string | null;
  error: string | null;
  /** Bumped by a restart, and part of the render key, so a restart remounts. */
  generation: number;
  /**
   * What this session was started with, and what a later turn overrides to.
   *
   * Held per pane rather than globally because two panes in one worktree are routinely a big model
   * and a cheap one — which is much of why several sessions at once is the feature.
   */
  model: string | null;
  effort: string | null;
  /** The permission or approval mode, in the provider's own spelling. */
  mode: string | null;
  /**
   * Whether this session was asked to run in the provider's high-speed mode.
   *
   * **What was asked for, not what is in force.** Only Claude has one, and whether it is actually
   * on depends on the account, the organization, the model, remaining credits and a rate limit
   * that can be in cooldown — none of which the frontend can see. The CLI reports the truth on
   * every turn and the Rust side turns a disagreement into a transcript notice, which is why this
   * flag is allowed to be the optimistic half of the pair.
   */
  fast: boolean;
  /**
   * True when `effort` has been changed to something the running session is not using.
   *
   * The one setting of the three that cannot be applied live. Claude reads `--effort` once at
   * startup and offers no control request for it, so the honest options were to lock the control
   * once a session starts, to restart behind the user's back, or to say so. This is saying so:
   * the value is kept, the control is marked, and Restart picks it up.
   *
   * Never set for Codex, which re-sends effort on every turn.
   */
  effortPending: boolean;
  /**
   * A provider the picker has been pointed at that the running session is not.
   *
   * **`kind` stays authoritative for the process.** Nothing else may read a provider from here —
   * the title, the transcript, the mode pill and the spawn all keep describing what is actually
   * running, because a pane that claimed to be Codex while a Claude process answered it would be
   * lying about the thing the user most needs to know.
   *
   * The same split as `effort`/`effortPending`, for the same reason: the provider is a property of
   * the process, and the process reads it once. One deliberate difference — this clears when the
   * selection comes back to the running provider, because "which agent" is a choice you can take
   * back, where an effort the session may or may not be using cannot be un-known.
   *
   * Null on a pane that has never been asked anything: `configure` restarts those outright rather
   * than marking them, since there is nothing to lose and a marker you have to act on is worse than
   * doing the obvious thing.
   */
  pendingProvider: string | null;
  /**
   * What this session can be asked to do by name, for the composer's `/` list.
   *
   * Empty until the provider says — Claude answers on its init line, Codex a few frames after the
   * thread opens — so an empty list means "not yet or none", and the composer treats both the same.
   */
  skills: AgentSkill[];
  /** Parent pane for an ephemeral `/btw` fork. Side panes are rendered as overlays, not splits. */
  sideOf: string | null;
  /** Live parent session for a WTM-owned delegated child. */
  parentSession: string | null;
  /** Sibling children created by one orchestration call share this id. */
  run: string | null;
  /** Human-facing task label supplied by the orchestrator. */
  agentTitle: string | null;
  /**
   * The provider says this session is out of usage, and the offer to continue elsewhere is standing.
   *
   * A register outside the transcript, like `usage` and `pendingProvider`: the banner needs the
   * *current* answer, and scanning a 20 000-event log backwards for the most recent limit on every
   * render is the cost `working` was moved out of the transcript to avoid.
   *
   * Cleared four ways — a turn starting (the limit lifted, and a turn that runs is the only proof of
   * that worth trusting), a restart, an accepted transfer, and a dismissal. Never by elapsed time:
   * that would need a timer, and there are none on this side.
   */
  limit: { message: string; resetsAt: number | null } | null;
  /**
   * The last event sequence this pane drew from the backend's replay buffer, or `null` when it
   * never repainted from one.
   *
   * Only `adopt` sets it. It exists because the frontend subscribes to `agent:event` *before* it
   * asks for the buffer, so an event emitted in between arrives twice — once live, once in the
   * snapshot — and comparing the emitter's own counter is the only way to tell that apart from a
   * session legitimately repeating a delta.
   */
  replayedThrough: number | null;
  /**
   * The id the *provider* knows this conversation by, once it has said.
   *
   * Distinct from `session`, which is this app's own handle and means nothing after a quit. This is
   * what a restored pane resumes with, and what a reload matches a live session against.
   */
  providerSession: string | null;
  /**
   * This pane was restored from the last run and has no process behind it.
   *
   * A pane rather than a row in a list, because the whole point is that the *arrangement* comes
   * back: a detached pane holds its place in the split tree so the surface looks the way it was
   * left, and offers to fill itself. Nothing spawns until the user asks or, for a shell, until the
   * worktree is looked at — see `materialise`.
   */
  detached: boolean;
}

/** One orchestration run, for the rail and the agents dialog. */
export type AgentRun = {
  parent: Pane | null;
  run: string;
  children: Pane[];
};

let nextPaneId = 0;

/** How many of these panes have a process behind them, or are on their way to one. */
function running(panes: readonly Pane[]): number {
  return panes.filter((pane) => !pane.detached).length;
}

/**
 * Where a worktree's surface is remembered between runs, keyed by worktree id.
 *
 * `localStorage`, like `wtm.worktrees.*` and `wtm.lastProject` beside it and for the same reason:
 * this is machine-local window state that only this window writes, and putting it in
 * `~/.config/wtm/` would invite copying a surface full of absolute paths to another machine along
 * with the preferences people do sync.
 */
const SURFACE_PREFIX = 'wtm.panes.';

/** One pane as it survives a quit: enough to put it back and offer to fill it. */
interface StoredPane {
  id: string;
  projectId: string;
  kind: SessionKind;
  /**
   * This app's own session handle.
   *
   * Meaningless after a quit — the process is gone — and load-bearing after a *reload*, where the
   * same backend is still running it. Matching on it is what lets a reload put a live session back
   * in the pane it was in rather than appending a new one.
   */
  session: string | null;
  /** What a restored agent pane resumes. Null for a shell, which has nothing to resume. */
  providerSession: string | null;
  /**
   * What the pane was last set to, so a resumed conversation comes back on the model it was on.
   *
   * No title beside them: `list_resumable` already carries one for every provider session and is
   * refreshed whenever a worktree is selected, so storing a second copy here would be a label that
   * could disagree with the list it sits next to.
   */
  model: string | null;
  effort: string | null;
  parentSession?: string | null;
  run?: string | null;
  agentTitle?: string | null;
}

/** A worktree's whole surface: the tree, what filled it, and where focus was. */
interface StoredSurface {
  layout: Layout | null;
  focused: string | null;
  panes: StoredPane[];
}

function readSurface(worktreeId: string): StoredSurface | null {
  try {
    const raw = localStorage.getItem(SURFACE_PREFIX + worktreeId);
    if (raw === null) return null;
    const parsed: unknown = JSON.parse(raw);
    // Shape-checked rather than trusted: this is the one input to the store that a previous
    // version of this app wrote, and a surface restored from a stale shape would put panes in the
    // tree that nothing can render.
    if (typeof parsed !== 'object' || parsed === null) return null;
    const surface = parsed as StoredSurface;
    if (!Array.isArray(surface.panes)) return null;
    return surface;
  } catch {
    /* Corrupt or unavailable. A forgotten surface is a worse morning than a wrong one is. */
    return null;
  }
}

function writeSurface(worktreeId: string, surface: StoredSurface): void {
  try {
    if (surface.panes.length === 0) localStorage.removeItem(SURFACE_PREFIX + worktreeId);
    else localStorage.setItem(SURFACE_PREFIX + worktreeId, JSON.stringify(surface));
  } catch {
    /* Quota or private mode. Persistence is a convenience, never a requirement. */
  }
}

/** Every worktree with a remembered surface. */
function storedWorktrees(): string[] {
  try {
    return Object.keys(localStorage)
      .filter((key) => key.startsWith(SURFACE_PREFIX))
      .map((key) => key.slice(SURFACE_PREFIX.length));
  } catch {
    return [];
  }
}

class Sessions {
  panes = $state<Pane[]>([]);
  /** One split tree per worktree, keyed by worktree id. */
  layouts = $state<Record<string, Layout | null>>({});
  /** Which pane last had focus, per worktree — the target a new split lands beside. */
  focused = $state<Record<string, string | null>>({});
  /** The agent catalogue, with availability. */
  options = $state<AgentOption[]>([]);
  /**
   * What each provider can do, by id, once asked.
   *
   * Fetched lazily and cached for the window's life: for Codex the answer costs a throwaway app
   * server, so asking per render would spawn a process per keystroke. Stale only if a CLI is
   * upgraded while wtm runs, which is worth a restart rather than a poll — and polling is banned.
   */
  capabilities = $state<Record<string, Capability | null>>({});
  /**
   * Providers whose last capability probe failed.
   *
   * Separate from `capabilities` because `null` there already means "in flight". Folding
   * failure into the same map made the picker spin on "reading capabilities…" forever and
   * blocked retry — `provider in capabilities` was true for a null that would never fill.
   */
  capabilityFailed = $state<Record<string, boolean>>({});
  /**
   * Conversations that can be picked up again, by worktree.
   *
   * Refreshed on demand and on selection rather than polled — the same policy `workspace` states, and
   * for the same reason: polling is how these tools end up spinning a fan.
   */
  resumable = $state<Record<string, Resumable[]>>({});
  /**
   * The files in each worktree, for the composer's `@` list.
   *
   * Cached because it shells out to `git ls-files`, and the alternative — asking per keystroke —
   * would spawn a process per character typed after an `@`. Filled on first use rather than on
   * open, so a session nobody references a file in never pays for it.
   *
   * Deliberately not refreshed on a filesystem event: there is no watcher here and adding one for
   * a typeahead would be a background cost for a convenience. It refreshes when a worktree is
   * reselected, which in practice is often enough — and a path this list has not heard of can
   * still be typed by hand.
   */
  files = $state<Record<string, string[]>>({});
  /** Stored plans, by worktree. */
  briefs = $state<Record<string, Brief[]>>({});
  /**
   * Background agents, by worktree.
   *
   * Read on demand and on window focus, because **there is no event when one finishes**. That means a
   * count can be a few seconds stale, which the UI says rather than hiding behind a timer — polling is
   * banned, and this is the honest alternative.
   */
  background = $state<Record<string, BackgroundTask[]>>({});
  error = $state<string | null>(null);
  /** True when a pane was asked for and a cap said no. Cleared by the next successful open. */
  atCapacity = $state(false);

  /**
   * What to call each delegated run, by run id.
   *
   * The rail used to label a run by its index in the rendered list, which was fine while nothing
   * ever removed one. Closing a run is now an ordinary thing for an orchestrator to do, and an
   * index renumbers Run 2 into Run 1 the moment Run 1 goes — so the label would name a different
   * batch than the one the user was just reading about.
   *
   * Assigned on first sight and never reclaimed: the numbers are a naming scheme, not a count, and
   * reusing a retired one is the same bug in slower motion. Not persisted, because `restore` walks
   * the stored panes in order and re-numbering them from one is indistinguishable from the
   * original assignment.
   */
  runOrdinals = $state<Record<string, number>>({});

  /**
   * Bumped whenever the user asks for a pane's focus, so the surface knows to move it.
   *
   * A counter, because two requests in a row must both be seen, and because the alternative is an
   * effect writing what it reads. Same mechanism `terminals` used, and for the same reason: an
   * effect that also tracked the selection would yank focus mid-arrow-key in the sidebar.
   */
  focusEpoch = $state(0);
  /** Which pane the last request was for. Not `$state`: read only when the epoch fires. */
  focusTarget: string | null = null;

  /*
   * What arrived about a session before any pane owned its id.
   *
   * Both of these exist because *opening* a session and *hearing from* it are two different
   * channels, and Rust has no reason to order them. `open_agent_session` returns the id only after
   * the spawn and the handshake write; the driver's own events go straight out to the webview from
   * wherever they are produced. For Claude the loss is not even a race — `ClaudeProtocol::open`
   * returns `Step::Ready` synchronously, so `agent:ready` is *always* emitted before the id has
   * crossed IPC. Matching on `paneBySession` alone dropped it every time, and since nothing else
   * ever sets `ready`, the pane said "starting…" for the rest of its life.
   *
   * Not `$state`: nothing renders them, and a reactive read here would make `record` — the hottest
   * path in the app — a dependency of whatever effect happened to be running.
   */

  /** Sessions that reported ready before their pane knew its id. */
  private readonly readyAhead = new Set<string>();
  /** Events that arrived before their pane knew its id, oldest first. */
  private readonly eventsAhead = new Map<
    string,
    { event: AgentEvent; seq: number | null }[]
  >();

  /**
   * The one status each worktree's sidebar row shows, keyed by worktree id. Absent means no dot.
   *
   * One `$derived` map rather than a `statusIn(id)` the sidebar calls per row: a method would re-scan
   * every pane once per rendered row on every pane change, where this scans once and every row reads
   * a key out of the result.
   *
   * **It does not read `pane.events`.** A `$state` proxy signals per property, so appending to the
   * log — which happens once per streamed token — does not invalidate this. That is the whole reason
   * `working` is a field rather than a fold over the log, and it is the property to preserve if this
   * map ever grows a new input.
   *
   * Replaces a `live` derived that nothing ever read.
   */
  statuses = $derived.by(() => {
    const out: Record<string, PaneStatus> = {};
    for (const pane of this.panes) {
      if (pane.sideOf !== null) continue;
      const next = statusOf(facts(pane));
      const held = out[pane.worktreeId];
      out[pane.worktreeId] = held === undefined ? next : worse(held, next);
    }
    return out;
  });

  /** The status of one pane, for its own header. */
  statusOfPane(pane: Pane): PaneStatus {
    return statusOf(facts(pane));
  }

  /** Delegated children of a session, in the order they were announced. */
  childrenOf(session: string | null): Pane[] {
    if (session === null) return [];
    return this.panes.filter((pane) => pane.parentSession === session);
  }

  /** Delegated children in a worktree, in announcement order. */
  delegatedIn(worktreeId: string): Pane[] {
    return this.panes.filter(
      (pane) => pane.worktreeId === worktreeId && pane.parentSession !== null,
    );
  }

  /**
   * Group those children by run, for the rail and the agents dialog.
   *
   * One place rather than two copies. The key is `run`, then the parent, then the pane — the
   * same fallback both views already used, so a child announced without a run id still sits
   * with its siblings instead of becoming its own group of one.
   */
  runsIn(worktreeId: string): AgentRun[] {
    const grouped = new Map<string, AgentRun>();
    for (const child of this.delegatedIn(worktreeId)) {
      const key = child.run ?? child.parentSession ?? child.id;
      const existing = grouped.get(key);
      if (existing) {
        existing.children.push(child);
        continue;
      }
      grouped.set(key, {
        parent: child.parentSession ? this.paneBySession(child.parentSession) : null,
        run: key,
        children: [child],
      });
    }
    return [...grouped.values()];
  }

  /** Catalogue label, or the raw provider id, or "Shell". */
  labelOf(pane: Pane): string {
    const kind = pane.kind;
    if (kind.kind !== 'agent') return 'Shell';
    return (
      this.options.find((option) => option.id === kind.provider)?.label ?? kind.provider
    );
  }

  /**
   * Give a run a name that survives its neighbours being closed.
   *
   * Idempotent, so `restore` can call it per stored pane without caring how many share a run.
   */
  private numberRun(run: string | null): void {
    if (run === null || run in this.runOrdinals) return;
    const used = Object.values(this.runOrdinals);
    this.runOrdinals = {
      ...this.runOrdinals,
      [run]: (used.length === 0 ? 0 : Math.max(...used)) + 1,
    };
  }

  /** What to call a run in the rail and the dialog. */
  runLabel(run: string): string {
    return `Run ${this.runOrdinals[run] ?? 1}`;
  }

  /**
   * A delegated child's pending approvals, so its orchestrator can answer them.
   *
   * # Why the parent answers at all
   *
   * Because a six-way fan-out otherwise costs six pane visits to get past six `Bash` prompts, and
   * the panes are not even on screen — a child holds no tile until it is selected. Nothing about
   * an approval requires its pane to be visible: it lives on `pane.approvals` and `answer` takes a
   * pane id, so the only thing that was missing is somewhere to render it.
   *
   * Ordered by child and then by that child's own queue, which is announcement order twice over.
   * Not by arrival time across the fan-out: a `PendingApproval` carries no timestamp, and adding
   * one to sort a list that is nearly always length one would be a field maintained for a tiebreak
   * nobody can perceive. Grouping a chatty child's prompts together is the better failure anyway.
   */
  delegatedApprovals(session: string | null): { pane: Pane; approval: PendingApproval }[] {
    return this.childrenOf(session).flatMap((pane) =>
      pane.approvals.map((approval) => ({ pane, approval })),
    );
  }

  /**
   * How many sessions are waiting on an answer, anywhere — including in another project.
   *
   * The dock badge reads this. It is the only indicator that reaches someone when wtm is not the
   * front application and notifications are off, and the only one that covers a project other than
   * the selected one: panes carry a `projectId`, but the sidebar only lists the active project's
   * worktrees, so sidebar dots alone still hide a blocked session one project over.
   */
  waitingCount = $derived(
    this.panes.filter((p) => p.sideOf === null && p.approvals.length > 0).length,
  );

  /** Whether any pane in a project wants attention, for the project switcher's rows. */
  wantsAttentionIn(projectId: string): boolean {
    return this.panes.some(
      (p) =>
        p.sideOf === null &&
        p.projectId === projectId &&
        (p.approvals.length > 0 || p.error !== null),
    );
  }

  paneById(id: string | null): Pane | null {
    if (id === null) return null;
    return this.panes.find((p) => p.id === id) ?? null;
  }

  paneBySession(session: string | null): Pane | null {
    if (session === null) return null;
    return this.panes.find((p) => p.session === session) ?? null;
  }

  panesIn(worktreeId: string | null): Pane[] {
    if (worktreeId === null) return [];
    return this.panes.filter((p) => p.worktreeId === worktreeId && p.sideOf === null);
  }

  /** The one dismissible side question belonging to a pane, if it has one. */
  sideFor(paneId: string): Pane | null {
    return this.panes.find((p) => p.sideOf === paneId) ?? null;
  }

  layoutFor(worktreeId: string | null): Layout | null {
    if (worktreeId === null) return null;
    return this.layouts[worktreeId] ?? null;
  }

  /** The worktree's shell panes, in creation order. */
  shellsIn(worktreeId: string | null): Pane[] {
    return this.panesIn(worktreeId).filter((p) => p.kind.kind === 'shell');
  }

  /**
   * Subscribe to every session event stream, and adopt what outlived a reload.
   *
   * Two things depend on this. The obvious one is that without it a reload leaks a CLI and a shell
   * per pane, because the session keeps running with nothing able to reach it. The other is the
   * transcript: Rust buffers each session's events for exactly this, so an adopted pane comes back
   * showing what it showed before rather than blank. See `agent_replay`.
   */
  async init(): Promise<UnlistenFn> {
    const offAgent = await listen<AgentEventEnvelope>('agent:event', (e) => {
      this.record(e.payload.session, e.payload.event, e.payload.seq);
    });
    const offAgentExit = await listen<AgentExit>('agent:exit', (e) => {
      this.noteExit(e.payload.session, e.payload.summary);
    });
    const offReady = await listen<AgentReady>('agent:ready', (e) => {
      const pane = this.paneBySession(e.payload.session);
      if (pane) pane.ready = true;
      else this.readyAhead.add(e.payload.session);
    });
    const offPtyExit = await listen<PtyExit>('pty:exit', (e) => {
      this.noteExit(e.payload.session, e.payload.summary);
    });
    const offSpawned = await listen<SpawnedSession>('agent:spawned', (e) => {
      this.adoptSpawned(e.payload);
    });
    const offReleased = await listen<{ sessions: string[] }>('agent:released', (e) => {
      this.dropReleased(e.payload.sessions);
    });

    await this.refreshOptions();

    const [shells, agents] = await Promise.all([
      commands.listTerminals().catch(() => []),
      commands.listAgentSessions().catch(() => []),
    ]);

    // Asked for up front so a picker is filled before anyone opens one. Not awaited: for Codex this
    // spawns a process, and a slow probe must not hold up the session list.
    for (const option of this.options.filter((o) => o.available)) {
      void this.loadCapability(option.id);
    }

    // Before adopting, so a live session can land in the pane it was in rather than being appended
    // beside an empty copy of itself.
    this.restore();

    for (const shell of shells) {
      await this.adopt({ kind: 'shell' }, shell.project, shell.worktree, shell.session);
    }
    // Sequentially, not `Promise.all`: each one fetches a transcript and then places a pane, and
    // `place` reads the tree it is about to rewrite. Interleaving them would race the layout.
    for (const agent of agents) {
      await this.adopt(
        { kind: 'agent', provider: agent.provider },
        agent.project,
        agent.worktree,
        agent.session,
        agent.providerSession,
      );
    }

    return () => {
      offAgent();
      offAgentExit();
      offReady();
      offPtyExit();
      offSpawned();
      offReleased();
    };
  }

  /**
   * Drop panes for sessions Rust has already ended — the mirror of `adoptSpawned`.
   *
   * No IPC close, because there is nothing left to close: `close_agents` ran on the socket thread
   * and the processes are gone by the time this fires. Calling `close` would send a second
   * teardown for a session id the app has already forgotten and put its error in the banner.
   */
  private dropReleased(released: string[]): void {
    // Descendants too. Rust's `close_agents` now closes the whole settled subtree, but a
    // grandchild the announcement missed would otherwise stay as a pane pointing at a parent
    // that is gone — the orphan `close()` exists to prevent. Extra ids get an IPC close in
    // case their process is still up; announced ones do not, because those processes are gone
    // and a second teardown puts its error in the banner.
    const closing = new Set(released);
    let grew = true;
    while (grew) {
      grew = false;
      for (const pane of this.panes) {
        if (
          pane.session !== null &&
          pane.parentSession !== null &&
          closing.has(pane.parentSession) &&
          !closing.has(pane.session)
        ) {
          closing.add(pane.session);
          grew = true;
        }
      }
    }
    const gone = this.panes.filter(
      (pane) => pane.session !== null && closing.has(pane.session),
    );
    for (const pane of gone) {
      if (pane.session && !released.includes(pane.session)) {
        void commands.closeAgentSession(pane.session).catch(() => {});
      }
    }
    if (gone.length === 0) return;

    const ids = new Set(gone.map((pane) => pane.id));
    this.panes = this.panes.filter((pane) => !ids.has(pane.id));

    // A released child usually holds no tile, but one that was shown or split does — and leaving a
    // leaf pointing at a pane that no longer exists is what `remove` is for.
    const layouts = { ...this.layouts };
    const focused = { ...this.focused };
    for (const pane of gone) {
      const pruned = remove(layouts[pane.worktreeId] ?? null, pane.id);
      layouts[pane.worktreeId] = pruned;
      if (focused[pane.worktreeId] === pane.id) {
        focused[pane.worktreeId] = panesOf(pruned).at(-1) ?? null;
      }
    }
    this.layouts = layouts;
    this.focused = focused;
    for (const worktreeId of new Set(gone.map((pane) => pane.worktreeId))) {
      this.remember(worktreeId);
      void this.refreshResumable(worktreeId);
    }
    this.clearCapacity();
  }

  /**
   * Show a pane for a session Rust started on its own — a handoff.
   *
   * # Why this ignores the pane cap
   *
   * Because the session is *already running*. The cap exists to stop someone opening a fifth CLI in
   * one worktree, and it refuses rather than evicting precisely so a running session is never
   * discarded. Refusing here would not prevent the fifth CLI; it would only leave it running with
   * nothing on screen able to reach it — the exact failure `adopt` exists to prevent after a reload.
   *
   * The pane is focused, which is deliberate and is the whole point of the feature. The user asked
   * one agent to consult another and the answer to "what is it doing" has to be visible without
   * hunting for it.
   */
  private adoptSpawned(spawned: SpawnedSession): void {
    // A second announcement for a session already on screen would open a duplicate pane pointed at
    // one CLI. Cheap to guard and impossible to notice if it ever happened.
    if (this.paneBySession(spawned.session)) return;

    const pane = this.blank(
      { kind: 'agent', provider: spawned.provider },
      spawned.project,
      spawned.worktree,
    );
    pane.model = spawned.model;
    pane.effort = spawned.effort;
    pane.mode = spawned.mode;
    pane.parentSession = spawned.parentSession;
    pane.run = spawned.run;
    pane.agentTitle = spawned.title;
    this.numberRun(pane.run);
    this.panes = [...this.panes, pane];

    const live = this.paneById(pane.id);
    if (live) this.claimSession(live, spawned.session);
    if (spawned.parentSession === null) {
      this.place(spawned.worktree, pane.id, 'right');
    } else {
      // Delegated children stay mounted but do not each consume a tile. The agent rail makes them
      // visible immediately and swaps one into the parent's tile when selected.
      this.remember(spawned.worktree);
    }
    void this.loadCapability(spawned.provider);
    // It is running, so it must stop being offered as resumable — the same correction `resume` makes.
    void this.refreshResumable(spawned.worktree);
    // A cap that refused a *user's* pane earlier should not keep saying so once this one appeared.
    this.clearCapacity();
  }

  /**
   * Ask a provider what it can do, once.
   *
   * An absent key is "not asked yet" (or "failed and cleared for retry"). `null` is a probe
   * in flight — claimed before the await so two panes do not spawn two Codex servers.
   * Failure is `capabilityFailed`, so the picker can offer Retry instead of spinning.
   */
  async loadCapability(provider: string): Promise<void> {
    if (this.capabilities[provider] != null) return;
    if (provider in this.capabilities && this.capabilities[provider] === null) return;
    // Claimed before the await, so two panes opening at once do not both spawn a probe.
    this.capabilities = { ...this.capabilities, [provider]: null };
    this.capabilityFailed = { ...this.capabilityFailed, [provider]: false };
    try {
      const capability = await commands.agentCapability(provider);
      this.capabilities = { ...this.capabilities, [provider]: capability };
    } catch (e) {
      this.error = errorMessage(e);
      const next = { ...this.capabilities };
      delete next[provider];
      this.capabilities = next;
      this.capabilityFailed = { ...this.capabilityFailed, [provider]: true };
    }
  }

  /** Drop a failed probe so `loadCapability` can ask again. */
  async retryCapability(provider: string): Promise<void> {
    const next = { ...this.capabilities };
    delete next[provider];
    this.capabilities = next;
    this.capabilityFailed = { ...this.capabilityFailed, [provider]: false };
    await this.loadCapability(provider);
  }

  /**
   * Change what a pane uses. Model and mode take effect on the running session; effort does not.
   *
   * The split is provider capability, not UI preference. Codex re-sends effort on each turn and
   * Cursor applies its advertised ACP config option, so both are live. Claude has control requests
   * for model and mode but effort is an argv flag read at startup, so only Claude marks it for the
   * next Restart.
   *
   * Optimistic: the pane shows the new value immediately and a provider that refuses the change
   * says so in the transcript as a `Notice`. The alternative — awaiting the round trip before the
   * pill updates — would make every menu selection feel like it did not register.
   */
  configure(
    paneId: string,
    next: {
      provider: string;
      model: string;
      effort: string;
      mode: string | null;
      fast: boolean;
    },
  ): void {
    const pane = this.paneById(paneId);
    if (!pane) return;
    // A shell has no picker, so there is nothing here that could have called this.
    const running = pane.kind.kind === 'agent' ? pane.kind.provider : null;
    if (running === null) return;

    const swapping = next.provider !== running;

    /*
     * A pane nobody has asked anything yet just becomes the other agent.
     *
     * Nothing to lose, so nothing to warn about — and a marker the user has to act on is strictly
     * worse than doing the obvious thing. `events.length === 0` is the wrong test: `session_ready`,
     * `skills_listed` and any stderr notice from the handshake are all in the log before the first
     * prompt, so a pane that has been open for two seconds is not empty by that measure.
     *
     * The model and effort are written first so the restart spawns with them; `restart` adopts
     * `pendingProvider` itself.
     */
    if (swapping) {
      const untouched = !pane.events.some(
        (e) => e.kind === 'user_echo' || e.kind === 'turn_started',
      );
      if (untouched) {
        pane.model = next.model;
        pane.effort = next.effort;
        // Only where the target can honour it. Codex and Cursor would ignore the flag rather than
        // fail on it, so carrying it would be harmless on the wire — but it would leave a pane
        // holding `fast: true` with no control to see or clear it, and swapping back to Claude
        // would then restore a setting the user never chose for that session.
        pane.fast = next.fast && this.capabilities[next.provider]?.supportsFast === true;
        pane.pendingProvider = next.provider;
        void this.restart(paneId);
        return;
      }
    }

    // A model can imply a mode (`opusplan` is Opus only in Plan mode — see the capability
    // table). Only on a genuine model change on the running provider, so a mode picked
    // afterwards wins and a pending cross-provider swap keeps describing the process that
    // exists. An assist, not a lock: nothing is forced back when the model changes away.
    const implied =
      !swapping && pane.model !== next.model
        ? (this.capabilities[next.provider]?.models.find((m) => m.id === next.model)
            ?.impliedMode ?? null)
        : null;
    const mode = implied ?? next.mode;

    const modelChanged = pane.model !== next.model;
    const effortChanged = pane.effort !== next.effort;
    const modeChanged = pane.mode !== mode;
    // Only where the provider claims one — a pane whose capability says no fast mode can still
    // reach here carrying `false`, and asking such a session to change a setting it does not have
    // is the `Notice`-in-the-transcript failure the model check below already guards against.
    const fastChanged =
      this.capabilities[running]?.supportsFast === true && pane.fast !== next.fast;
    const effortIsLive = running === 'codex' || running === 'cursor';
    // Sticky once set: a pane whose effort is already pending must not un-mark itself because a
    // later change happened to land back on the value the session was started with.
    if (effortChanged && pane.session !== null && !effortIsLive) pane.effortPending = true;
    if (effortIsLive) pane.effortPending = false;
    // Not sticky, unlike effort: pointing the picker back at the running agent is a retraction, and
    // there is nothing left pending once it agrees with the process again.
    pane.pendingProvider = swapping ? next.provider : null;

    pane.model = next.model;
    pane.effort = next.effort;
    pane.mode = mode;
    if (fastChanged) pane.fast = next.fast;

    // Nothing to say to a session that does not exist yet — `openAgentSession` will carry these
    // as spawn arguments instead.
    //
    // And nothing to say to one whose vocabulary these values are not in: a Claude process asked to
    // use `gpt-5.6-sol` answers with a `Notice`, which lands in the transcript as a real error
    // report for a change the user never made. A pending swap is applied by Restart, not by
    // telling the wrong process about it.
    const liveEffortChanged = effortIsLive && effortChanged;
    if (
      pane.session === null ||
      swapping ||
      (!modelChanged && !liveEffortChanged && !modeChanged && !fastChanged)
    )
      return;
    void commands
      .configureSession(
        pane.session,
        modelChanged ? next.model : null,
        liveEffortChanged ? next.effort : null,
        modeChanged ? mode : null,
        fastChanged ? next.fast : null,
      )
      .catch((e: unknown) => {
        this.error = errorMessage(e);
      });
  }

  /**
   * Turn high-speed mode on or off, changing nothing else.
   *
   * Separate from `configure` rather than a call into it, because `configure` takes the picker's
   * *whole* selection and assigns every field from it. A caller that only knows about fast — the
   * composer's `/fast` — would have to invent values for model, effort and mode, and the honest
   * ones are nullable: a pane whose capability has landed can still be holding `effort: null` from
   * before it did. Passing `''` for that would register as an effort change, mark the pane "on
   * restart", and leave a marker referring to a change nobody made.
   *
   * Returns false when the provider has no high-speed mode, so the caller can say so rather than
   * appearing to succeed at nothing.
   */
  setFast(paneId: string, fast: boolean): boolean {
    const pane = this.paneById(paneId);
    if (!pane || pane.kind.kind !== 'agent') return false;
    if (this.capabilities[pane.kind.provider]?.supportsFast !== true) return false;
    if (pane.fast === fast) return true;

    pane.fast = fast;
    // Nothing to tell a session that does not exist yet — the spawn carries `pane.fast` instead.
    if (pane.session === null) return true;
    void commands
      .configureSession(pane.session, null, null, null, fast)
      .catch((e: unknown) => {
        this.error = errorMessage(e);
      });
    return true;
  }

  /**
   * Fill the `@` list for a worktree. Silent on failure, like every auxiliary read here.
   *
   * Not guarded against a second call the way `loadCapability` is, because this one is cheap,
   * idempotent, and genuinely wants to re-run: reselecting a worktree is how a list that predates
   * a `git checkout` gets corrected.
   */
  async loadFiles(worktreeId: string): Promise<void> {
    try {
      const files = await commands.listWorktreeFiles(worktreeId);
      this.files = { ...this.files, [worktreeId]: files };
    } catch {
      /* The composer works without it — the user types the path. */
    }
  }

  /**
   * What can be resumed in a worktree. Silent on failure, like every auxiliary read here.
   *
   * The await comes **before** the map is touched, and that ordering is the fix rather than a style
   * choice. This used to read `this.resumable` in the spread of an object literal whose value was
   * the await — and object properties evaluate in source order, so the read happened ahead of the
   * suspension and therefore inside the tracking window of whatever effect called this. The three
   * methods here are called from one. See `patch`.
   */
  async refreshResumable(worktreeId: string): Promise<void> {
    try {
      const list = await commands.listResumable(worktreeId);
      const next = patch(this.resumable, worktreeId, list);
      if (next) this.resumable = next;
    } catch {
      /* Deliberately silent. A resume list that cannot be read is not worth a banner. */
    }
  }

  /**
   * Pick up a conversation.
   *
   * The model and effort come from the record rather than from the provider's defaults: resuming on a
   * different model than the conversation was held on is a surprise, and one the transcript above
   * would not explain.
   */
  async resume(projectId: string, worktreeId: string, record: Resumable): Promise<void> {
    if (!this.hasRoom(worktreeId)) return;

    const pane = this.blank(
      { kind: 'agent', provider: record.provider },
      projectId,
      worktreeId,
    );
    pane.model = record.model;
    pane.effort = record.effort;
    this.panes = [...this.panes, pane];
    this.place(worktreeId, pane.id, 'right');
    void this.loadCapability(record.provider);

    try {
      const session = await commands.openAgentSession({
        projectId,
        worktreeId,
        agentId: record.provider,
        options: {
          model: record.model,
          effort: record.effort,
          resume: record.providerSession,
        },
      });
      await this.claimOrClose(pane.id, session, 'agent');
      // It is running now, so it must stop being offered — `list_resumable` excludes live sessions,
      // and refreshing is what makes the list agree with the screen.
      void this.refreshResumable(worktreeId);
    } catch (e) {
      const live = this.paneById(pane.id);
      if (live) live.error = errorMessage(e);
    }
  }

  /** Stop offering a conversation. The explicit discard, unlike closing a pane. */
  async forget(worktreeId: string, record: Resumable): Promise<void> {
    try {
      await commands.forgetSession(record.provider, record.providerSession);
    } catch (e) {
      this.error = errorMessage(e);
    }
    await this.refreshResumable(worktreeId);
  }

  /** Stored plans for a worktree. Silent on failure, and await-then-assign — see `refreshResumable`. */
  async refreshBriefs(projectId: string, worktreeId: string): Promise<void> {
    try {
      const list = await commands.listBriefs(projectId, worktreeId);
      const next = patch(this.briefs, worktreeId, list);
      if (next) this.briefs = next;
    } catch {
      /* Deliberately silent. */
    }
  }

  /**
   * Background agents for a worktree. Empty when there is no `claude` on the PATH.
   *
   * The most expensive of the three by a wide margin — it shells out to `claude agents`, and the
   * Claude Code binary is a few hundred megabytes — which is why this was the arm of the loop that
   * turned a re-render into an unusable machine. Await-then-assign, see `refreshResumable`.
   */
  async refreshBackground(worktreeId: string): Promise<void> {
    try {
      const list = await commands.listBackgroundTasks(worktreeId);
      const next = patch(this.background, worktreeId, list);
      if (next) this.background = next;
    } catch {
      /* Deliberately silent. */
    }
  }

  /**
   * Answer an approval, keeping the plan if one was approved.
   *
   * The plan is stored *before* the answer goes out, and the order is deliberate: allowing an
   * `ExitPlanMode` lets the session move on and start editing, and a plan captured after that has
   * already been superseded by the work it describes.
   */
  async answerAndKeep(
    paneId: string,
    requestId: string,
    answer: ApprovalAnswer,
    request: ApprovalRequest,
  ): Promise<void> {
    const pane = this.paneById(paneId);
    if (
      pane &&
      request.kind === 'plan_review' &&
      answer.kind !== 'deny' &&
      request.markdown.trim().length > 0
    ) {
      try {
        await commands.saveBrief({
          projectId: pane.projectId,
          worktreeId: pane.worktreeId,
          provider: pane.kind.kind === 'agent' ? pane.kind.provider : 'unknown',
          markdown: request.markdown,
          model: pane.model,
          providerPath: request.path,
        });
        void this.refreshBriefs(pane.projectId, pane.worktreeId);
      } catch (e) {
        // Surfaced but not fatal: failing to keep a copy must not block the approval the user just
        // gave, and the provider has written its own copy either way.
        this.error = errorMessage(e);
      }
    }
    // An approved plan takes the CLI out of plan mode, and it announces nothing when it goes —
    // `permission_mode_changed` is telemetry, not a stream event. The landing mode depends on
    // `updatedPermissions` this side never sees, so the only honest pill is the "session hasn't
    // said" sentinel, not a stale "Plan" and never a guess.
    if (pane && request.kind === 'plan_review' && answer.kind !== 'deny') {
      pane.mode = null;
    }
    await this.answer(paneId, requestId, answer);
  }

  /** Forget a stored plan. */
  async forgetBrief(projectId: string, worktreeId: string, id: string): Promise<void> {
    try {
      await commands.removeBrief(projectId, id);
    } catch (e) {
      this.error = errorMessage(e);
    }
    await this.refreshBriefs(projectId, worktreeId);
  }

  /**
   * Hand a plan to a new session.
   *
   * The handoff the user asked for, in its simplest honest form: open a session on the provider they
   * pick and send the plan as its first turn. The heavier mechanism — an agent doing this itself —
   * needs wtm to expose tools to the CLIs, and one config entry already covers the Claude-to-Codex
   * direction without it.
   */
  async handOff(
    projectId: string,
    worktreeId: string,
    provider: string,
    brief: Brief,
  ): Promise<void> {
    await this.openAgent(projectId, worktreeId, provider, 'right');
    const pane = this.panesIn(worktreeId).at(-1);
    if (!pane) return;

    // The turn is sent before the handshake finishes; every provider queues it and echoes it, so the
    // prompt is visibly in the transcript rather than appearing to vanish.
    const prompt = `Review this plan and say what you would change.\n\n---\n\n${brief.markdown}`;
    // `send` waits for the session id itself, and says so when it gives up. This used to be an
    // inline copy of that wait which returned silently on timeout — the one place in the increment
    // that added it that failed without telling anyone.
    await this.send(pane.id, prompt);
  }

  /**
   * Carry a limited session's conversation to the other provider and keep working.
   *
   * # Why a new pane rather than a restart
   *
   * `restart` is the mechanism that already changes a pane's provider, and it is the wrong one here
   * for two reasons. It clears the transcript — which is the thing being transferred, and losing it
   * mid-transfer would be unrecoverable — and it ends the old session, whose conversation is still
   * resumable once the limit lifts. So the old pane is left exactly as it is, and the continuation
   * opens beside it: two panes, one conversation, and the option of going back.
   *
   * The digest is built here and sent as an ordinary first turn, the same shape as `handOff`. See
   * `../transfer` for what goes into it and what is deliberately left out.
   */
  async continueOn(paneId: string, provider: string): Promise<void> {
    const source = this.paneById(paneId);
    if (!source) return;

    const kind = source.kind;
    const fromLabel =
      kind.kind === 'agent'
        ? (this.options.find((o) => o.id === kind.provider)?.label ?? kind.provider)
        : 'another agent';
    // Read before the await: `openAgent` can evict nothing, but the pane's log keeps growing while
    // the new session starts, and the transfer should be of the conversation as the user saw it.
    const prompt = transferPrompt(source.events, fromLabel);

    await this.openAgent(source.projectId, source.worktreeId, provider, 'right', paneId);
    const pane = this.panesIn(source.worktreeId).at(-1);
    // `hasRoom` refuses at the cap, in which case no pane was created and `atCapacity` is already
    // set for the UI to explain. Bailing out leaves the offer standing, which is right: nothing has
    // been transferred.
    if (!pane || pane.id === paneId) return;

    const sent = await this.send(pane.id, prompt);
    // Only on a turn that was actually accepted. A failed spawn leaves the banner up, so the user
    // can try the other provider or wait the limit out rather than being told the work moved when it
    // did not.
    if (sent) {
      const live = this.paneById(paneId);
      if (live) live.limit = null;
    }
  }

  /** Put the limit offer away without taking it. The session stays limited; the banner goes. */
  dismissLimit(paneId: string): void {
    const pane = this.paneById(paneId);
    if (pane) pane.limit = null;
  }

  /**
   * Re-probe which agents this machine has, and which this repository offers.
   *
   * Silent on failure — an auxiliary convenience. Called at startup without a project and again
   * whenever the selection lands on a new one, because `offered` is the repository's answer and
   * changes with it.
   *
   * Assigned only when something moved, for the reason `patch` gives: a freshly built array with
   * identical contents is a **new identity**, and this list feeds `SessionPane`'s `label` derived —
   * so an unconditional assignment re-renders every pane header for nothing. Called from an effect,
   * which is the other half of why it matters.
   */
  async refreshOptions(projectId?: string | null): Promise<void> {
    try {
      const next = await commands.listAgents(projectId);
      if (JSON.stringify(this.options) !== JSON.stringify(next)) this.options = next;
    } catch {
      /* Deliberately silent, as `workspace.refreshOpeners` is and for the same reason. */
    }
  }

  /**
   * Put every remembered surface back, as panes with nothing behind them yet.
   *
   * # Why the arrangement is restored and the sessions are not
   *
   * `wtm_config::sessions` calls its file a resume list rather than a session list, and the reason
   * still holds: re-establishing every conversation on launch would fork a CLI per pane for
   * conversations the user may be finished with. What that argument never covered is the
   * *arrangement*. A split you spent a minute building is not a process, costs nothing to put back,
   * and losing it on every quit — or on an update, which is a quit — was the complaint.
   *
   * So the tree comes back with its panes in it, and each pane offers to fill itself. A shell fills
   * itself when the worktree is looked at, because a login shell has nothing to resume and nothing
   * to decide; an agent waits to be asked, because resuming picks a conversation.
   */
  private restore(): void {
    const panes: Pane[] = [];
    const layouts: Record<string, Layout | null> = {};
    const focused: Record<string, string | null> = {};

    for (const worktreeId of storedWorktrees()) {
      const surface = readSurface(worktreeId);
      if (!surface) continue;

      // The per-worktree cap is re-applied rather than trusted, because a stored surface may have
      // been written by a build with different limits and the cap is a statement about how many
      // panes fit on one screen — as true of a restored pane as of a new one.
      //
      // `MAX_PANES` deliberately is **not** applied here. That one bounds OS threads and event
      // subscriptions, and a detached pane has neither; applying it across every remembered
      // worktree would silently drop panes from whichever ones `storedWorktrees` happened to
      // return last. It is enforced where a process is actually created instead — `materialise`
      // and `reattach` both check `hasRoom`.
      const roots = surface.panes
        .filter((pane) => !pane.parentSession)
        .slice(0, MAX_PANES_PER_WORKTREE);
      const children = surface.panes.filter((pane) => pane.parentSession).slice(0, 20);
      const kept = [...roots, ...children];

      let layout = surface.layout;
      const keptIds = new Set(kept.map((pane) => pane.id));
      for (const stored of surface.panes.filter((pane) => !keptIds.has(pane.id))) {
        layout = remove(layout, stored.id);
      }
      if (kept.length === 0) continue;

      for (const stored of kept) {
        const pane = this.blank(stored.kind, stored.projectId, worktreeId);
        // Its own id back, not a fresh one: the stored tree names panes by id, so minting new ones
        // would leave every leaf pointing at nothing.
        pane.id = stored.id;
        pane.providerSession = stored.providerSession;
        pane.model = stored.model;
        pane.effort = stored.effort;
        pane.parentSession = stored.parentSession ?? null;
        pane.run = stored.run ?? null;
        pane.agentTitle = stored.agentTitle ?? null;
        // Re-numbered in stored order, which is the order they were announced in — so a restored
        // rail reads the same as the one that was written, without the ordinals being persisted.
        this.numberRun(pane.run);
        pane.detached = true;
        panes.push(pane);
      }
      layouts[worktreeId] = layout;
      // Only if it survived the cap. Focusing a pane that was dropped would leave the worktree with
      // no focused pane and a split that lands in the wrong place on the next open.
      focused[worktreeId] = kept.some((p) => p.id === surface.focused)
        ? surface.focused
        : null;
    }

    if (panes.length === 0) return;

    // Past every id we just took back, so the next `blank` cannot mint one that collides with a
    // restored pane and silently join two leaves of the tree.
    nextPaneId = panes.reduce((highest, pane) => {
      const n = Number.parseInt(pane.id.replace('pane-', ''), 10);
      return Number.isFinite(n) ? Math.max(highest, n) : highest;
    }, nextPaneId);

    this.panes = [...this.panes, ...panes];
    this.layouts = { ...this.layouts, ...layouts };
    this.focused = { ...this.focused, ...focused };
  }

  /**
   * Fill in a restored worktree's shells, once it is the one being looked at.
   *
   * Called from the selection effect rather than from `restore`, so a launch with six remembered
   * worktrees spawns nothing until one of them is opened. Agents are deliberately left alone: they
   * render an offer to resume, and which conversation to resume is a choice.
   */
  async materialise(projectId: string, worktreeId: string): Promise<void> {
    const waiting = this.panesIn(worktreeId).filter(
      (pane) => pane.detached && pane.kind.kind === 'shell',
    );
    for (const pane of waiting) {
      // Checked per shell rather than once, because each one that spawns changes the answer. A
      // refusal leaves the pane detached and sets `atCapacity`, which is the same way every other
      // over-cap request is reported.
      if (!this.canFill(worktreeId)) return;
      pane.detached = false;
      await this.fillShell(pane, projectId, worktreeId);
    }
  }

  /**
   * Resume the conversation a restored agent pane was left holding.
   *
   * In place: the pane keeps its id and its position in the tree, which is the difference between
   * this and picking the same conversation out of the resume list, where it would open beside
   * whatever had focus.
   */
  async reattach(paneId: string): Promise<void> {
    const pane = this.paneById(paneId);
    if (!pane?.detached || pane.kind.kind !== 'agent') return;
    if (!this.canFill(pane.worktreeId)) return;
    pane.detached = false;
    pane.error = null;

    try {
      const session = await commands.openAgentSession({
        projectId: pane.projectId,
        worktreeId: pane.worktreeId,
        agentId: pane.kind.provider,
        options: {
          model: pane.model,
          effort: pane.effort,
          mode: pane.mode,
          fast: pane.fast,
          resume: pane.providerSession,
        },
      });
      await this.claimOrClose(pane.id, session, 'agent');
      this.error = null;
      void this.refreshResumable(pane.worktreeId);
    } catch (e) {
      const live = this.paneById(pane.id);
      if (live) {
        live.error = errorMessage(e);
        // Back to offering, so a failed resume can be tried again rather than leaving a pane that
        // is neither running nor asking for anything.
        live.detached = true;
      }
    }
  }

  /**
   * Put an already-running session back on screen, with everything it has already said.
   *
   * The transcript is replayed through `record` rather than assigned to `pane.events`, because the
   * transcript is not the only thing a reload lost: whether the session is mid-turn, what it is
   * blocked on, its usage, its skill list and its limit banner are all tracked *beside* the log and
   * are rebuilt only by the same pass that wrote them. Assigning the array would give back a pane
   * that reads correctly and behaves as though nothing were running.
   */
  private async adopt(
    kind: SessionKind,
    projectId: string,
    worktreeId: string,
    session: string,
    providerSession?: string,
  ): Promise<void> {
    // A reload restores the surface first, so this session probably already has a pane waiting for
    // it. Matched on wtm's own session id before the provider's, because only the first is exact:
    // the backend is the same process across a reload, so the handle it minted is still the handle
    // it minted, whereas a provider id is empty until the handshake and shared with any fork.
    const restored =
      this.panesIn(worktreeId).find((p) => p.detached && p.session === session) ??
      (providerSession
        ? this.panesIn(worktreeId).find(
            (p) => p.detached && p.providerSession === providerSession,
          )
        : undefined);

    const pane = restored ?? this.blank(kind, projectId, worktreeId);
    if (restored) {
      restored.detached = false;
      // Cleared so the claim below is not skipped as a session it already owns.
      restored.session = null;
    } else {
      // Registered before it is claimed, unlike the rest of this function's history. `claimSession`
      // drains anything buffered for the id by handing it to `record`, and `record` finds its pane
      // by searching `this.panes` — so claiming an unregistered pane would quietly re-buffer
      // instead of delivering. Looking the pane back up is also what gets the `$state` proxy rather
      // than the raw object, which is why every other caller here does the same.
      this.panes = [...this.panes, pane];
    }
    const live = this.paneById(pane.id);
    if (live) {
      // A shell's scrollback lives in its xterm instance and is replayed by the pty bridge, so only
      // an agent has anything to fetch here.
      const buffered =
        kind.kind === 'agent' ? await commands.agentReplay(session).catch(() => []) : [];

      // Assigned directly rather than through `claimSession`, which would drain the events that
      // arrived while the fetch was in flight — and those must land *after* the snapshot, not
      // before it. `claimSession` runs below, once `replayedThrough` can tell the two apart.
      live.session = session;
      for (const { seq, event } of buffered) this.record(session, event, seq);
      live.replayedThrough = buffered.at(-1)?.seq ?? null;

      this.claimSession(live, session);
      // Adopted, so it is running by definition — readiness was announced before this window
      // existed and there is no second announcement coming.
      live.ready = true;
    }
    // A restored pane is already in the tree, in the place the user put it. Placing it again would
    // move it, which is the behaviour this whole path exists to stop.
    if (!restored) this.place(worktreeId, pane.id, 'right');
    else this.remember(worktreeId);
  }

  private blank(kind: SessionKind, projectId: string, worktreeId: string): Pane {
    nextPaneId += 1;
    return {
      id: `pane-${nextPaneId}`,
      projectId,
      worktreeId,
      kind,
      session: null,
      events: [],
      eventBytes: 0,
      approvals: [],
      usage: null,
      working: false,
      lastTurnFinished: false,
      unseen: false,
      ready: false,
      ended: null,
      error: null,
      generation: 0,
      model: null,
      effort: null,
      mode: null,
      fast: false,
      effortPending: false,
      pendingProvider: null,
      skills: [],
      sideOf: null,
      parentSession: null,
      run: null,
      agentTitle: null,
      limit: null,
      replayedThrough: null,
      providerSession: null,
      detached: false,
    };
  }

  /**
   * Put a new pane into a worktree's tree and focus it.
   *
   * # Why `beside` is a parameter and not just the focus map
   *
   * Because the focus map is stale exactly when it matters. The only click-driven writer is a
   * **bubble-phase** `onclick` on the pane's `<section>`, which runs *after* the split button's own
   * handler — and macOS WebKit does not focus a `<button>` on click, so the `onfocusin` beside it
   * does not cover for that. Clicking Split on a pane that was not already focused therefore
   * inserted the new pane beside whichever pane *was*, which is not where the button was.
   *
   * So a caller that knows its target says so. The fallback is kept for the callers that genuinely
   * do not have one — a resume, an adopt, a handoff — where "beside whatever was last focused" is
   * the only answer available.
   */
  private place(
    worktreeId: string,
    paneId: string,
    placement: Placement,
    beside?: string | null,
  ): void {
    this.layouts = {
      ...this.layouts,
      [worktreeId]: insert(
        this.layoutFor(worktreeId),
        paneId,
        beside ?? this.focused[worktreeId] ?? null,
        placement,
      ),
    };
    this.focus(worktreeId, paneId);
    this.remember(worktreeId);
  }

  /** Show an already-running delegated session in the current tile or in an explicit split. */
  showRelated(paneId: string, split = false): void {
    const pane = this.paneById(paneId);
    if (!pane) return;
    const layout = this.layoutFor(pane.worktreeId);
    if (panesOf(layout).includes(paneId)) {
      this.focus(pane.worktreeId, paneId);
      return;
    }
    if (split) {
      if (!this.hasTileRoom(pane.worktreeId)) return;
      this.place(pane.worktreeId, paneId, 'right');
      return;
    }

    const parent = pane.parentSession ? this.paneBySession(pane.parentSession) : null;
    const tiles = panesOf(layout);
    // Intersected with the layout rather than trusted. `focused` is written by `place` and by the
    // arrow keys and is never validated against the tree, so it can name a pane that has since
    // been closed — and `replacePane` rewrites nothing when its `from` is absent, returning a
    // new-but-identical tree. The symptom was a rail item that highlighted and did nothing.
    const focused = this.focused[pane.worktreeId];
    const target =
      (focused !== null && focused !== undefined && tiles.includes(focused)
        ? focused
        : null) ?? (parent && tiles.includes(parent.id) ? parent.id : tiles.at(0));
    if (!target) {
      if (!this.hasTileRoom(pane.worktreeId)) return;
      this.place(pane.worktreeId, paneId, 'right');
      return;
    }
    this.layouts = {
      ...this.layouts,
      [pane.worktreeId]: replacePane(layout, target, paneId),
    };
    this.focus(pane.worktreeId, paneId);
    this.remember(pane.worktreeId);
  }

  /**
   * Whether another pane can be opened here. Reports rather than throws, so callers can explain.
   *
   * # Why this counts tiles rather than pane records
   *
   * Because that is what the per-worktree cap is a statement about — how many panes fit on a
   * screen — and the two stopped being the same thing when delegation shipped. A delegated child
   * is a pane record with no layout node: it is deliberately not given a tile, and the agent rail
   * is how it is reached. `panesIn` cannot see that distinction, so a `spawn_agents` run of three
   * children in a worktree showing one session counted as four panes and refused every subsequent
   * Shell, agent and resume in that worktree — silently, because a refusal returns rather than
   * raising. `panesOf` counts leaves of the layout, which excludes children and `/btw` side panes
   * for free while still counting a restored *detached* pane, which does hold a tile.
   *
   * The global cap keeps counting processes, because that is what *it* is a statement about — OS
   * threads and event subscriptions. But it counts only panes the user opened: `ARCHITECTURE.md`
   * §8 already carves out the exception that a delegated run "may own up to twenty child processes
   * because that count is the requested feature", and that bound belongs where it is enforced, in
   * `handoff.rs`'s `MAX_TASKS`, rather than being applied a second time here against a different
   * budget. Without this the eighth child of one fan-out locked pane creation app-wide.
   */
  private hasRoom(worktreeId: string): boolean {
    const room =
      panesOf(this.layoutFor(worktreeId)).length < MAX_PANES_PER_WORKTREE &&
      running(this.ownPanes()) < MAX_PANES;
    this.noteCapacity(room);
    return room;
  }

  /**
   * Whether another *tile* fits. Not a process: splitting a delegated child does not start one.
   *
   * `hasRoom` also checks the global process cap, which would refuse a split in a window that
   * already has eight user-opened CLIs even though this call adds none.
   */
  private hasTileRoom(worktreeId: string): boolean {
    const room = panesOf(this.layoutFor(worktreeId)).length < MAX_PANES_PER_WORKTREE;
    this.noteCapacity(room, AT_TILE_CAP);
    return room;
  }

  /**
   * Whether a pane that is already on screen may be given a process.
   *
   * Distinct from `hasRoom` because a detached pane is already counted by it, so a worktree
   * restored at its per-worktree limit would refuse to fill even the first of its own panes. What
   * the two caps are *for* is what separates them: `MAX_PANES_PER_WORKTREE` bounds how many panes
   * fit on a screen and this pane already has its place, while `MAX_PANES` bounds OS threads and
   * event subscriptions, which is exactly what filling one adds.
   *
   * Both counts exclude delegated children for the reasons `hasRoom` records, and the per-worktree
   * one intersects the layout with the pane list rather than reading `panesIn`: this one is asking
   * how many tiles already have a process behind them.
   */
  private canFill(worktreeId: string): boolean {
    const tiles = new Set(panesOf(this.layoutFor(worktreeId)));
    const filled = this.panes.filter((pane) => tiles.has(pane.id));
    const room =
      running(filled) < MAX_PANES_PER_WORKTREE && running(this.ownPanes()) < MAX_PANES;
    this.noteCapacity(room);
    return room;
  }

  /** Panes the user opened. Delegated children are budgeted in `handoff.rs`; see `hasRoom`. */
  private ownPanes(): Pane[] {
    return this.panes.filter((pane) => pane.parentSession === null);
  }

  /**
   * Record a cap's answer where the user can see it.
   *
   * The flag alone was not enough, and the way it failed is worth keeping: its one reader lived
   * inside `SessionSurface`'s `{#if !activeLayout}` empty state — a branch that renders only when
   * the worktree has no panes at all, which is the one situation in which you cannot be at the
   * cap. So the explanation was unreachable in exactly the case it was written for, and a refused
   * click did nothing at all. `error` has a banner that is always mounted; the flag stays for the
   * empty state's own copy.
   */
  private noteCapacity(room: boolean, message: string = AT_CAPACITY): void {
    this.atCapacity = !room;
    if (!room) this.error = message;
  }

  /**
   * Room appeared, so stop saying there is none.
   *
   * Only when the banner is still *this* message. The error slot is shared with every other
   * failure the store reports, and closing a pane is not evidence that a failed spawn or an
   * unreadable config has been dealt with.
   */
  private clearCapacity(): void {
    this.atCapacity = false;
    if (this.error === AT_CAPACITY || this.error === AT_TILE_CAP) this.error = null;
  }

  /**
   * Open a shell in the worktree. Not idempotent — several in one worktree is the point.
   *
   * It used to focus an existing shell instead of opening a second one, on the grounds that a
   * second `$SHELL -l` in one directory is never what anyone means. It routinely is: a dev server
   * in one and `git` in another is the ordinary way to work, and having to leave the app for it
   * was the single most-asked-about limitation of the dock. Focusing is still what ⌘J does, and
   * that now lives in `focusOrOpenShell` where the caller can choose.
   *
   * `beside` names the pane the new one lands next to, as in `openAgent`.
   */
  async openShell(
    projectId: string,
    worktreeId: string,
    placement: Placement = 'below',
    beside?: string,
  ): Promise<void> {
    if (!this.hasRoom(worktreeId)) return;

    const pane = this.blank({ kind: 'shell' }, projectId, worktreeId);
    // Appended before the spawn is asked for: the pane mounts a terminal with a null session, which
    // is what makes the shell's first prompt unlosable. See `Terminal.svelte`.
    this.panes = [...this.panes, pane];
    this.place(worktreeId, pane.id, placement, beside);
    await this.fillShell(pane, projectId, worktreeId);
  }

  /**
   * Spawn a login shell for a pane that is already on screen.
   *
   * Split out of `openShell` for `materialise`, which needs the second half only: a restored shell
   * pane already has its place in the tree and must keep it, so it must not go through `place`.
   */
  private async fillShell(
    pane: Pane,
    projectId: string,
    worktreeId: string,
  ): Promise<void> {
    try {
      const session = await commands.openTerminal({
        projectId,
        worktreeId,
        rows: SPAWN_ROWS,
        cols: SPAWN_COLS,
      });
      await this.claimOrClose(pane.id, session, 'shell');
    } catch (e) {
      const live = this.paneById(pane.id);
      if (live) live.error = errorMessage(e);
    }
  }

  /**
   * What ⌘J does: reach a shell here, opening one only if there is none.
   *
   * Three cases, and the third is the interesting one. With several shells open, repeating the
   * shortcut *cycles* them rather than focusing the same one forever or opening an endless supply
   * — which is the keyboard half of what a tab strip would give, without a stacked layout node.
   *
   * Cycling follows the layout's visual order rather than creation order, because the user is
   * looking at tiles, not at a list. `panesOf` walks the tree left-to-right, top-to-bottom.
   */
  async focusOrOpenShell(projectId: string, worktreeId: string): Promise<void> {
    const shells = this.shellsIn(worktreeId);
    if (shells.length === 0) {
      await this.openShell(projectId, worktreeId);
      return;
    }

    const order = panesOf(this.layoutFor(worktreeId));
    const visual = order
      .map((id) => shells.find((pane) => pane.id === id))
      .filter((pane): pane is Pane => pane !== undefined);
    const ordered = visual.length === shells.length ? visual : shells;

    // Only advance from a shell. Coming from an agent pane the intent is "get me to a terminal",
    // and skipping the first one because the focus map happened to point elsewhere would read as
    // the shortcut losing a shell.
    const current = this.focused[worktreeId] ?? null;
    const at = ordered.findIndex((pane) => pane.id === current);
    const next = ordered[at === -1 ? 0 : (at + 1) % ordered.length];
    if (next) this.focus(worktreeId, next.id);
  }

  /**
   * Start an agent session. Not idempotent — several in one worktree is the feature.
   *
   * `beside` names the pane the new one lands next to. Pass it whenever the caller knows; see
   * `place` for what goes wrong when it does not.
   */
  async openAgent(
    projectId: string,
    worktreeId: string,
    provider: string,
    placement: Placement = 'right',
    beside?: string,
  ): Promise<void> {
    if (!this.hasRoom(worktreeId)) return;

    const pane = this.blank({ kind: 'agent', provider }, projectId, worktreeId);
    // The provider's own defaults, so a session starts on the model it would have chosen and the
    // picker shows that rather than an empty control. `null` until the capability lands, which the
    // picker resolves to the default itself.
    const capability = this.capabilities[provider];
    const preferred = capability?.models.find((m) => m.isDefault) ?? capability?.models[0];
    pane.model = preferred?.id ?? null;
    pane.effort = preferred?.defaultEffort ?? null;
    // Left null for Claude, whose capability marks no mode as the default — it deliberately passes
    // no flag so the user's own settings decide, and `session_ready` reports back what they said.
    // Codex does mark one, because there the mode is two protocol fields that wtm has to send.
    pane.mode = capability?.modes.find((m) => m.isDefault)?.id ?? null;

    this.panes = [...this.panes, pane];
    this.place(worktreeId, pane.id, placement, beside);
    void this.loadCapability(provider);

    try {
      const session = await commands.openAgentSession({
        projectId,
        worktreeId,
        agentId: provider,
        options: {
          model: pane.model,
          effort: pane.effort,
          mode: pane.mode,
          fast: pane.fast,
        },
      });
      await this.claimOrClose(pane.id, session, 'agent');
      this.error = null;
    } catch (e) {
      const live = this.paneById(pane.id);
      if (live) live.error = errorMessage(e);
    }
  }

  focus(worktreeId: string, paneId: string): void {
    this.focused = { ...this.focused, [worktreeId]: paneId };
    this.focusTarget = paneId;
    this.focusEpoch += 1;
    this.seen(paneId);
    this.remember(worktreeId);
  }

  /** Note focus without asking for it, so a click does not re-trigger the focus effect. */
  noteFocus(worktreeId: string, paneId: string): void {
    this.focused = { ...this.focused, [worktreeId]: paneId };
    this.seen(paneId);
    this.remember(worktreeId);
  }

  /**
   * Forget that anything happened in this worktree unseen.
   *
   * **"Looked at" means the worktree became the selected one**, not that a pane took focus. Every
   * pane in the selected worktree is on screen — `SessionSurface` hides by worktree, not by pane — so
   * a four-way split where one pane finished is a thing the user is already looking at, and a dot
   * demanding a click would be asking them to acknowledge something in front of them.
   *
   * Guarded on the flag so the common case — nothing unseen, which is most selections — signals no
   * reader at all.
   */
  markSeen(worktreeId: string | null): void {
    if (worktreeId === null) return;
    for (const pane of this.panes) {
      if (pane.worktreeId === worktreeId && pane.unseen) pane.unseen = false;
    }
    attention.clear(worktreeId);
  }

  /** Clear one pane's unread mark. Guarded, so an ordinary click writes nothing. */
  private seen(paneId: string): void {
    const pane = this.paneById(paneId);
    if (pane?.unseen) pane.unseen = false;
  }

  /**
   * Reposition a pane that is already open.
   *
   * A move is a layout edit and nothing else: no session is started or ended, so `hasRoom` is not
   * consulted and no cap applies. `move` refuses anything that would be a no-op and returns the tree
   * it was given, so the identity check is what keeps a drop-where-it-already-was from signalling
   * every reader for nothing.
   *
   * `noteFocus` rather than `focus`: the caller decides where the caret lands, because the pointer path
   * and the keyboard path want different answers — a drag should hand focus to the pane, an arrow key
   * has to hand it back to the grip so the next press works.
   */
  movePane(worktreeId: string, paneId: string, target: Target): void {
    const layout = this.layoutFor(worktreeId);
    const next = move(layout, paneId, target);
    if (next === layout) return;
    this.layouts = { ...this.layouts, [worktreeId]: next };
    this.noteFocus(worktreeId, paneId);
    this.remember(worktreeId);
  }

  setRatio(worktreeId: string, path: string, ratio: number): void {
    const layout = this.layoutFor(worktreeId);
    if (!layout) return;
    this.layouts = { ...this.layouts, [worktreeId]: resize(layout, path, ratio) };
    this.remember(worktreeId);
  }

  /**
   * Wait for a pane's session id, or give up.
   *
   * A pane exists before its session does — `blank` sets `session: null`, and `openAgentSession`
   * fills it in a moment later — so a turn composed in that window has nowhere to go. Waiting is
   * what makes typing into a pane that is still starting work at all, which is the ordinary thing
   * to do: the composer is on screen and focused from the moment the pane opens.
   *
   * Bounded, because a spawn that failed will never produce one. Null covers all three ways this
   * ends badly — the pane went away, the spawn errored, the session ended — so the caller has one
   * case to handle rather than four.
   */
  private async awaitSession(paneId: string): Promise<Pane | null> {
    for (let attempt = 0; attempt < SESSION_WAIT_TRIES; attempt += 1) {
      const pane = this.paneById(paneId);
      if (!pane || pane.error !== null || pane.ended !== null) return null;
      if (pane.session !== null) return pane;
      await new Promise((resolve) => setTimeout(resolve, SESSION_WAIT_STEP_MS));
    }
    return null;
  }

  /**
   * Send a turn, waiting for the session id if it has not landed yet.
   *
   * **Reports whether the turn was accepted**, so the composer can hold the draft until it was.
   * This returned `void` and dropped the text on the floor whenever `session` was still null, which
   * is every message typed into a fresh pane before the CLI finished starting — and the composer,
   * having already cleared itself, left no sign that anything had happened at all.
   */
  async send(
    paneId: string,
    text: string,
    attachments: AgentAttachment[] = [],
  ): Promise<boolean> {
    const pane = await this.awaitSession(paneId);
    if (!pane?.session) {
      this.error = 'That session never started, so the message was not sent.';
      return false;
    }
    try {
      await commands.sendTurn(pane.session, text, attachments);
      return true;
    } catch (e) {
      this.error = errorMessage(e);
      return false;
    }
  }

  /**
   * Fork a live conversation for `/btw` without adding either side of the exchange to its log.
   *
   * The hidden pane is intentional: it reuses the ordinary event/session plumbing, but has no
   * layout node. `SessionPane` renders it as a dismissible card owned by the parent instead.
   */
  async openSide(
    parentId: string,
    question: string,
    attachments: AgentAttachment[] = [],
  ): Promise<boolean> {
    const parent = await this.awaitSession(parentId);
    if (!parent?.session || parent.kind.kind !== 'agent') {
      this.error = 'That conversation is not ready for a side question yet.';
      return false;
    }

    const previous = this.sideFor(parentId);
    if (previous) await this.close(previous.id);

    const side = this.blank(parent.kind, parent.projectId, parent.worktreeId);
    side.sideOf = parent.id;
    side.model = parent.model;
    side.effort = parent.effort;
    side.mode = parent.mode;
    // Inherited, unlike the delegation path in `handoff.rs`, and the difference is fan-out: a `/btw`
    // is one short turn on the conversation the user is already looking at, where one handoff can
    // open twenty children. Matching the parent is what keeps the side answer comparable to the
    // ones above it.
    side.fast = parent.fast;
    this.panes = [...this.panes, side];

    try {
      const session = await commands.openAgentSideSession({
        parentSession: parent.session,
        options: {
          model: side.model,
          effort: side.effort,
          mode: side.mode,
          fast: side.fast,
        },
      });
      const live = this.paneById(side.id);
      if (!live) {
        await commands.closeAgentSession(session);
        return false;
      }
      this.claimSession(live, session);
      if (question.length > 0 || attachments.length > 0) {
        await commands.sendTurn(session, question, attachments);
      }
      this.error = null;
      return true;
    } catch (e) {
      const live = this.paneById(side.id);
      if (live) live.error = errorMessage(e);
      return false;
    }
  }

  async interrupt(paneId: string): Promise<void> {
    const pane = this.paneById(paneId);
    if (!pane?.session) return;
    try {
      await commands.interruptTurn(pane.session);
    } catch (e) {
      this.error = errorMessage(e);
    }
  }

  /**
   * Answer an approval.
   *
   * Removed locally as well as by the `approval_resolved` that follows, so the card disappears on
   * click rather than after a round trip. Safe for the same reason `toggleFavorite` is optimistic:
   * nothing else has an opinion, and the authoritative removal arrives moments later and agrees.
   */
  async answer(paneId: string, requestId: string, answer: ApprovalAnswer): Promise<void> {
    const pane = this.paneById(paneId);
    if (!pane?.session) return;
    const request = pane.approvals.find((a) => a.id === requestId);
    pane.approvals = pane.approvals.filter((a) => a.id !== requestId);
    try {
      await commands.answerApproval(pane.session, requestId, answer);
    } catch (e) {
      this.error = errorMessage(e);
      // Put the card back so a failed round trip is retryable rather than stranding the
      // turn with no control attached to it.
      const live = this.paneById(paneId);
      if (live && request && !live.approvals.some((a) => a.id === requestId)) {
        live.approvals = [request, ...live.approvals];
      }
    }
  }

  /**
   * End a session and drop its pane.
   *
   * The only thing that discards a transcript *and the pane with it* — `restart` also clears the
   * transcript, but keeps the pane and its position, and leaves the conversation resumable.
   *
   * # Why this takes the delegated children with it
   *
   * Because nothing else can reach them once it has gone. A child holds no tile of its own, so the
   * routes to one are the rail and the agents dialog, and both are drawn from the *parent* — an
   * orphan is a live CLI with no control attached to it anywhere in the window, discovered later as
   * a process in `ps`. Closing an orchestrator is also an unambiguous statement about the work: the
   * conversation that asked for those reviews is over.
   *
   * Depth-first, so a child that ran its own delegation takes its grandchildren too. The `/btw`
   * side pane is the same shape on a different link — `sideOf`, not `parentSession` — and without
   * this it survived as a CLI whose overlay had unmounted with the parent.
   */
  async close(paneId: string): Promise<void> {
    const closing = this.paneById(paneId);
    const side = this.sideFor(paneId);
    if (side) await this.close(side.id);
    for (const child of this.childrenOf(closing?.session ?? null)) {
      await this.close(child.id);
    }
    await this.closeOne(paneId);
  }

  private async closeOne(paneId: string): Promise<void> {
    const pane = this.paneById(paneId);
    if (!pane) return;

    if (pane.session) {
      try {
        if (pane.kind.kind === 'shell') await commands.closeTerminal(pane.session);
        else await commands.closeAgentSession(pane.session);
      } catch (e) {
        this.error = errorMessage(e);
      }
    }

    this.panes = this.panes.filter((p) => p.id !== paneId);
    this.layouts = {
      ...this.layouts,
      [pane.worktreeId]: remove(this.layoutFor(pane.worktreeId), paneId),
    };
    // The conversation is no longer running, so it can be offered again.
    if (pane.kind.kind === 'agent') void this.refreshResumable(pane.worktreeId);
    if (this.focused[pane.worktreeId] === paneId) {
      const survivors = panesOf(this.layoutFor(pane.worktreeId));
      this.focused = {
        ...this.focused,
        [pane.worktreeId]: survivors.at(-1) ?? null,
      };
    }
    this.clearCapacity();
    this.remember(pane.worktreeId);
  }

  /** End the old session if it is still running, then start a fresh one in a fresh pane. */
  async restart(paneId: string): Promise<void> {
    const pane = this.paneById(paneId);
    if (!pane) return;

    if (pane.session && pane.ended === null) {
      try {
        if (pane.kind.kind === 'shell') await commands.closeTerminal(pane.session);
        else await commands.closeAgentSession(pane.session);
      } catch {
        /* Already gone is the ordinary case here. */
      }
    }

    pane.generation += 1;
    pane.session = null;
    pane.events = [];
    pane.eventBytes = 0;
    pane.approvals = [];
    pane.usage = null;
    // A fresh session has no turn in flight and nothing you have not seen. Both would otherwise
    // survive the reset and describe a process that no longer exists.
    pane.working = false;
    pane.lastTurnFinished = false;
    pane.unseen = false;
    pane.ready = false;
    pane.ended = null;
    pane.error = null;
    // The offer belonged to a conversation that no longer exists. A restarted session may well hit
    // the same limit on its first turn, and then it says so again.
    pane.limit = null;
    // The whole point of the marker: the new process is spawned with the effort the picker is
    // showing, so the two now agree and the "restart to apply" hint has been satisfied.
    pane.effortPending = false;
    // Re-announced by the new session, and a stale list is worse than none — a restarted pane may
    // be a different provider's, and skills are worktree-scoped.
    pane.skills = [];

    /*
     * The one place a pane's provider changes, and it is here because this is the only moment it
     * can: the provider *is* the process, so pointing a pane at a different agent means replacing
     * the process, which is what a restart already does.
     *
     * The mode is dropped rather than carried. A mode is provider vocabulary — `acceptEdits` means
     * nothing to Codex and `full-access` nothing to Claude — so the new session takes the target's
     * own default (`ProviderEntry::default_mode`, or the repository's) and reports back what it
     * resolved to on `session_ready`. Model and effort are already the target's: the picker set them
     * when it set `pendingProvider`.
     */
    if (pane.pendingProvider !== null && pane.kind.kind === 'agent') {
      pane.kind = { kind: 'agent', provider: pane.pendingProvider };
      pane.pendingProvider = null;
      pane.mode = null;
      void this.loadCapability(pane.kind.provider);
    }

    this.focus(pane.worktreeId, pane.id);

    try {
      const session =
        pane.kind.kind === 'shell'
          ? await commands.openTerminal({
              projectId: pane.projectId,
              worktreeId: pane.worktreeId,
              rows: SPAWN_ROWS,
              cols: SPAWN_COLS,
            })
          : await commands.openAgentSession({
              projectId: pane.projectId,
              worktreeId: pane.worktreeId,
              agentId: pane.kind.provider,
              options: {
                model: pane.model,
                effort: pane.effort,
                mode: pane.mode,
                fast: pane.fast,
              },
            });
      await this.claimOrClose(pane.id, session, pane.kind.kind);
    } catch (e) {
      const live = this.paneById(pane.id);
      if (live) live.error = errorMessage(e);
    }

    // The conversation this just ended is no longer running, so it can be offered again. That is
    // what makes a one-click Restart defensible: the transcript on screen is discarded, but the
    // conversation itself reappears under "Pick up where you left off".
    if (pane.kind.kind === 'agent') void this.refreshResumable(pane.worktreeId);
  }

  /**
   * Drop panes for worktrees that are no longer in `ids`.
   *
   * Removal through the app already ends a worktree's sessions before teardown runs. This is for the
   * other route — `git worktree remove` in a real terminal, which wtm notices on the next window
   * focus — where a session would otherwise survive with its working directory unlinked.
   *
   * Returns early when nothing moved, because the caller is an effect that reads `panes`.
   */
  reconcile(projectId: string, ids: string[]): void {
    const alive = new Set(ids);
    const doomed = this.panes.filter(
      (p) => p.projectId === projectId && !alive.has(p.worktreeId),
    );
    if (doomed.length === 0) return;

    for (const pane of doomed) {
      if (!pane.session) continue;
      if (pane.kind.kind === 'shell') {
        void commands.closeTerminal(pane.session).catch(() => {});
      } else {
        void commands.closeAgentSession(pane.session).catch(() => {});
      }
    }
    const gone = new Set(doomed.map((p) => p.id));
    this.panes = this.panes.filter((p) => !gone.has(p.id));

    const layouts = { ...this.layouts };
    for (const pane of doomed) delete layouts[pane.worktreeId];
    this.layouts = layouts;
    this.clearCapacity();
    // A worktree that has been removed outside the app is not coming back, so its surface goes too
    // — otherwise the next launch restores panes for a directory that no longer exists.
    for (const worktreeId of new Set(doomed.map((pane) => pane.worktreeId))) {
      this.remember(worktreeId);
    }
  }

  /**
   * Write a worktree's surface down, so the next run puts it back.
   *
   * Called from every operation that changes the arrangement rather than from an effect. An effect
   * would have to read `panes`, `layouts` and `focused` — all three of which these same operations
   * write — and `layout.svelte.ts` and `patch` both document what reading and writing one array in
   * one effect does here.
   *
   * Side panes are excluded: a `/btw` fork is a single-turn overlay that is never resumable, and
   * restoring one would offer to continue a conversation the provider was told not to keep.
   */
  private remember(worktreeId: string): void {
    const panes = this.panesIn(worktreeId).filter((pane) => pane.sideOf === null);
    writeSurface(worktreeId, {
      layout: this.layouts[worktreeId] ?? null,
      focused: this.focused[worktreeId] ?? null,
      panes: panes.map((pane) => ({
        id: pane.id,
        projectId: pane.projectId,
        kind: pane.kind,
        session: pane.session,
        providerSession: pane.providerSession,
        model: pane.model,
        effort: pane.effort,
        parentSession: pane.parentSession,
        run: pane.run,
        agentTitle: pane.agentTitle,
      })),
    });
  }

  /**
   * Bind a backend session to a pane, or close it if the pane vanished during the await.
   *
   * Every open path that used to drop the id when `live` was null leaked a CLI. `openSide`
   * already closed in that case; this is that rule in one place.
   */
  private async claimOrClose(
    paneId: string,
    session: string,
    kind: 'shell' | 'agent',
  ): Promise<void> {
    const live = this.paneById(paneId);
    if (live) {
      this.claimSession(live, session);
      return;
    }
    try {
      if (kind === 'shell') await commands.closeTerminal(session);
      else await commands.closeAgentSession(session);
    } catch {
      /* Already gone is the ordinary case here. */
    }
  }

  /**
   * Give a pane its backend session id, and hand it whatever already arrived for that id.
   *
   * Every site that learns a session id goes through here rather than assigning `pane.session`
   * directly. That is the whole mechanism: the assignment and the drain have to be one step, or
   * there is a window between them in which an event is dropped for the same reason it was dropped
   * before — see `readyAhead`.
   */
  private claimSession(pane: Pane, session: string): void {
    pane.session = session;

    if (this.readyAhead.delete(session)) pane.ready = true;

    const waiting = this.eventsAhead.get(session);
    if (waiting) {
      this.eventsAhead.delete(session);
      for (const held of waiting) this.record(session, held.event, held.seq);
    }
    this.remember(pane.worktreeId);
  }

  /** Hold an event for a session no pane owns yet. Oldest dropped first, as the transcript does. */
  private holdEvent(session: string, event: AgentEvent, seq: number | null): void {
    const waiting = this.eventsAhead.get(session) ?? [];
    waiting.push({ event, seq });
    if (waiting.length > MAX_EARLY_EVENTS)
      waiting.splice(0, waiting.length - MAX_EARLY_EVENTS);
    this.eventsAhead.set(session, waiting);
  }

  private record(session: string, event: AgentEvent, seq: number | null = null): void {
    const pane = this.paneBySession(session);
    if (!pane) {
      // Not noise to discard: a CLI that is not logged in says so on stderr during the handshake,
      // which is exactly the window where no pane owns the id yet. `session.rs` calls a silent
      // session with no transcript the worst possible presentation of that failure, and dropping
      // these was how it happened.
      this.holdEvent(session, event, seq);
      return;
    }

    // Already drawn from the replay snapshot this pane repainted with. See `Pane.replayedThrough`.
    if (seq !== null && pane.replayedThrough !== null && seq <= pane.replayedThrough)
      return;

    /*
     * Tracked outside the log as well as in it. The log is what the transcript renders; this is what
     * the pane is *blocked on* and whether it is *busy*, and folding the whole log on every append
     * would be O(events) per delta on the hottest path in the app.
     *
     * `working` is the second thing held under that argument, and it replaced a backward scan that
     * used to live in `SessionPane`. Every arm below is O(1); `announce` allocates, but only on an
     * approval, a finished turn or a failure — never on a `message_delta` or a `command_output`, so a
     * chatty tool adds nothing.
     *
     * Reading the selection from `attention` is safe here, and the reason is worth stating because
     * `patch` and `SessionSurface` both document the trap: this is a `listen()` callback, not an
     * effect body, so a read cannot make an effect depend on a write.
     */
    if (event.kind === 'turn_started') {
      pane.working = true;
      pane.lastTurnFinished = false;
      // A turn that starts is the only trustworthy evidence a limit has lifted, and it costs nothing
      // to read it that way — the alternative is a countdown against `resetsAt`, which needs a timer
      // and would clear the offer while the provider was still refusing.
      pane.limit = null;
    } else if (event.kind === 'turn_finished') {
      pane.working = false;
      pane.lastTurnFinished = true;
      if (pane.sideOf === null && attention.announce('finished', announceable(pane))) {
        pane.unseen = true;
      }
      // A side question is single-turn. Keep its answer in the overlay, not a spare CLI process.
      if (pane.sideOf !== null && pane.session) {
        void commands.closeAgentSession(pane.session);
      }
    } else if (event.kind === 'failed') {
      pane.working = false;
      if (pane.sideOf === null && attention.announce('failed', announceable(pane))) {
        pane.unseen = true;
      }
    } else if (event.kind === 'limit_reached') {
      pane.working = false;
      pane.limit = { message: event.message, resetsAt: event.resetsAt };
      if (pane.sideOf === null && attention.announce('limit', announceable(pane))) {
        pane.unseen = true;
      }
    }

    if (event.kind === 'approval_requested') {
      pane.approvals = [...pane.approvals, { id: event.id, request: event.request }];
      if (
        pane.sideOf === null &&
        attention.announce(announcementFor(event.request), announceable(pane))
      ) {
        pane.unseen = true;
      }
    } else if (event.kind === 'approval_resolved') {
      pane.approvals = pane.approvals.filter((a) => a.id !== event.id);
    } else if (event.kind === 'skills_listed') {
      // Replaced, not merged: a provider that answers twice is correcting itself, and a skill
      // deleted from disk should leave the list rather than linger because it was once there.
      pane.skills = event.skills;
    } else if (event.kind === 'session_ready') {
      // What a restored pane will resume with. Recorded here because this is the first moment it
      // exists — Claude chooses it on its init line, Codex assigns it when the thread opens.
      pane.providerSession = event.providerSessionId;
      this.remember(pane.worktreeId);
      // The one setting wtm can learn rather than choose. Claude passes no `--permission-mode`
      // precisely so `~/.claude/settings.json` decides, so without adopting the answer the mode
      // pill would show a default the session is not in.
      if (event.mode !== null) pane.mode = event.mode;
    } else if (event.kind === 'usage') {
      pane.usage = {
        tokensIn: event.tokensIn,
        tokensOut: event.tokensOut,
        cached: event.cached,
        contextUsed: event.contextUsed,
        // The last window we were told, when this update does not carry one. Claude reports the
        // window only when a turn ends but reports the footprint on every request, so dropping it
        // here would blank the meter's denominator between turns — a live numerator with no
        // denominator reads as "—", which is worse than the stale-but-correct window.
        contextWindow: event.contextWindow ?? pane.usage?.contextWindow ?? null,
      };
    } else if (event.kind === 'turn_finished') {
      pane.usage = {
        ...event.usage,
        contextWindow: event.usage.contextWindow ?? pane.usage?.contextWindow ?? null,
      };
    }

    let appended = true;
    if (event.kind === 'patch') {
      // A Codex patch update is the complete current diff, not another historical action. Replace
      // the preceding snapshot in place so hundreds of cumulative full-file strings cannot remain
      // in one pane. Searching backwards normally examines only the events since the last update.
      for (let index = pane.events.length - 1; index >= 0; index -= 1) {
        const previous = pane.events[index];
        if (previous?.kind === 'patch' && previous.id === event.id) {
          pane.eventBytes -= eventBytes(previous);
          pane.events.splice(index, 1);
          pane.events.push(event);
          pane.eventBytes += eventBytes(event);
          appended = false;
          break;
        }
      }
    }

    if (appended) {
      pane.events.push(event);
      pane.eventBytes += eventBytes(event);
    }

    let remove = Math.max(0, pane.events.length - MAX_EVENTS);
    let bytes = pane.eventBytes;
    for (let index = 0; index < remove; index += 1) {
      bytes -= eventBytes(pane.events[index]!);
    }
    while (remove < pane.events.length && bytes > MAX_EVENT_BYTES) {
      bytes -= eventBytes(pane.events[remove]!);
      remove += 1;
    }
    if (remove > 0) {
      pane.events.splice(0, remove);
      pane.eventBytes = Math.max(0, bytes);
    }
  }

  private noteExit(session: string, summary: string): void {
    const pane = this.paneBySession(session);
    if (!pane) return;
    pane.ended = summary;
    pane.ready = false;
    pane.working = false;
    /*
     * News if it happened out of sight — but no notification and no toast.
     *
     * `close` removes the pane before its exit arrives, so in principle everything reaching here is
     * unexpected and worth saying. In practice a `restart` can race it, and a toast about a session
     * the user restarted a moment ago would be a lie about a thing they did on purpose. The dot is
     * cheap enough to be wrong; an alert is not.
     */
    if (pane.sideOf === null && attention.offScreen(pane.worktreeId)) pane.unseen = true;
    // An approval nobody can answer any more would sit on screen forever.
    pane.approvals = [];
  }
}

/**
 * The parts of a pane an announcement needs.
 *
 * A module function rather than a method, so `attention` takes a structural argument and neither
 * store imports the other's types — which is what keeps `sessions → attention → workspace` a chain
 * rather than a cycle.
 */
/** A pane, as the fields `statusOf` reads. See `status.ts` for why the argument is structural. */
function facts(pane: Pane): Parameters<typeof statusOf>[0] {
  return {
    agent: pane.kind.kind === 'agent',
    detached: pane.detached,
    ready: pane.ready,
    ended: pane.ended,
    error: pane.error,
    approvals: pane.approvals,
    working: pane.working,
    unseen: pane.unseen,
  };
}

function announceable(pane: Pane): Announceable {
  return {
    id: pane.id,
    projectId: pane.projectId,
    worktreeId: pane.worktreeId,
    provider: pane.kind.kind === 'agent' ? pane.kind.provider : null,
  };
}

/**
 * Which announcement an approval is.
 *
 * A question is not a permission gate, and saying "waiting on an approval" about
 * `AskUserQuestion` made every notification read the same. `tool_input` stays an approval
 * deliberately: it is the "unknown tool wants to run" fallback, an allow/deny card.
 */
function announcementFor(request: ApprovalRequest): Announcement {
  switch (request.kind) {
    case 'user_input':
      return 'question';
    case 'plan_review':
      return 'plan';
    default:
      return 'approval';
  }
}

export const sessions = new Sessions();
