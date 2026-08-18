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
}

let nextPaneId = 0;

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
  private readonly eventsAhead = new Map<string, AgentEvent[]>();

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
   * Adopting matters even though an adopted pane comes back with an empty transcript: Rust buffers
   * nothing, so there is no history to restore. What it prevents is a session running with nothing
   * able to reach it — without this, a reload during `just dev` leaks a CLI and a shell per pane.
   */
  async init(): Promise<UnlistenFn> {
    const offAgent = await listen<AgentEventEnvelope>('agent:event', (e) => {
      this.record(e.payload.session, e.payload.event);
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

    for (const shell of shells) {
      this.adopt({ kind: 'shell' }, shell.project, shell.worktree, shell.session);
    }
    for (const agent of agents) {
      this.adopt(
        { kind: 'agent', provider: agent.provider },
        agent.project,
        agent.worktree,
        agent.session,
      );
    }

    return () => {
      offAgent();
      offAgentExit();
      offReady();
      offPtyExit();
      offSpawned();
    };
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
    this.panes = [...this.panes, pane];

    const live = this.paneById(pane.id);
    if (live) this.claimSession(live, spawned.session);
    this.place(spawned.worktree, pane.id, 'right');
    void this.loadCapability(spawned.provider);
    // It is running, so it must stop being offered as resumable — the same correction `resume` makes.
    void this.refreshResumable(spawned.worktree);
    // A cap that refused a *user's* pane earlier should not keep saying so once this one appeared.
    this.atCapacity = false;
  }

  /**
   * Ask a provider what it can do, once.
   *
   * `null` in the map means "asked and it failed", which the picker renders as a warning — distinct
   * from an absent key, which means "not asked yet" and renders as a spinner. Collapsing those two
   * would make a logged-out CLI look like a slow one.
   */
  async loadCapability(provider: string): Promise<void> {
    if (provider in this.capabilities) return;
    // Claimed before the await, so two panes opening at once do not both spawn a probe.
    this.capabilities = { ...this.capabilities, [provider]: null };
    try {
      const capability = await commands.agentCapability(provider);
      this.capabilities = { ...this.capabilities, [provider]: capability };
    } catch (e) {
      this.error = errorMessage(e);
    }
  }

  /**
   * Change what a pane uses. Model and mode take effect on the running session; effort does not.
   *
   * The split is not arbitrary and it is not the same on both providers. Codex re-sends model,
   * effort and both permission fields on every `turn/start`, so all of it is live there. Claude has
   * control requests for the model and the mode and none for effort, which is an argv flag read at
   * startup — so effort is stored, flagged, and applied by the next Restart.
   *
   * Optimistic: the pane shows the new value immediately and a provider that refuses the change
   * says so in the transcript as a `Notice`. The alternative — awaiting the round trip before the
   * pill updates — would make every menu selection feel like it did not register.
   */
  configure(
    paneId: string,
    next: { provider: string; model: string; effort: string; mode: string | null },
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
    // Sticky once set: a pane whose effort is already pending must not un-mark itself because a
    // later change happened to land back on the value the session was started with.
    if (effortChanged && pane.session !== null && running !== 'codex')
      pane.effortPending = true;
    if (running === 'codex') pane.effortPending = false;
    // Not sticky, unlike effort: pointing the picker back at the running agent is a retraction, and
    // there is nothing left pending once it agrees with the process again.
    pane.pendingProvider = swapping ? next.provider : null;

    pane.model = next.model;
    pane.effort = next.effort;
    pane.mode = mode;

    // Nothing to say to a session that does not exist yet — `openAgentSession` will carry these
    // as spawn arguments instead.
    //
    // And nothing to say to one whose vocabulary these values are not in: a Claude process asked to
    // use `gpt-5.6-sol` answers with a `Notice`, which lands in the transcript as a real error
    // report for a change the user never made. A pending swap is applied by Restart, not by
    // telling the wrong process about it.
    const liveEffortChanged = running === 'codex' && effortChanged;
    if (
      pane.session === null ||
      swapping ||
      (!modelChanged && !liveEffortChanged && !modeChanged)
    )
      return;
    void commands
      .configureSession(
        pane.session,
        modelChanged ? next.model : null,
        liveEffortChanged ? next.effort : null,
        modeChanged ? mode : null,
      )
      .catch((e: unknown) => {
        this.error = errorMessage(e);
      });
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
      const live = this.paneById(pane.id);
      if (live) this.claimSession(live, session);
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

  /** Put an already-running session back on screen. */
  private adopt(
    kind: SessionKind,
    projectId: string,
    worktreeId: string,
    session: string,
  ): void {
    const pane = this.blank(kind, projectId, worktreeId);
    // Registered before it is claimed, unlike the rest of this function's history. `claimSession`
    // drains anything buffered for the id by handing it to `record`, and `record` finds its pane by
    // searching `this.panes` — so claiming an unregistered pane would quietly re-buffer instead of
    // delivering. Looking the pane back up is also what gets the `$state` proxy rather than the raw
    // object, which is why every other caller here does the same.
    this.panes = [...this.panes, pane];
    const live = this.paneById(pane.id);
    if (live) {
      this.claimSession(live, session);
      // Adopted, so it is running by definition — readiness was announced before this window
      // existed and there is no second announcement coming.
      live.ready = true;
    }
    this.place(worktreeId, pane.id, 'right');
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
      approvals: [],
      usage: null,
      working: false,
      unseen: false,
      ready: false,
      ended: null,
      error: null,
      generation: 0,
      model: null,
      effort: null,
      mode: null,
      effortPending: false,
      pendingProvider: null,
      skills: [],
      sideOf: null,
      limit: null,
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
  }

  /** Whether another pane can be opened here. Reports rather than throws, so callers can explain. */
  private hasRoom(worktreeId: string): boolean {
    const room =
      this.panesIn(worktreeId).length < MAX_PANES_PER_WORKTREE &&
      this.panes.length < MAX_PANES;
    this.atCapacity = !room;
    return room;
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

    try {
      const session = await commands.openTerminal({
        projectId,
        worktreeId,
        rows: SPAWN_ROWS,
        cols: SPAWN_COLS,
      });
      const live = this.paneById(pane.id);
      if (live) this.claimSession(live, session);
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
        options: { model: pane.model, effort: pane.effort, mode: pane.mode },
      });
      const live = this.paneById(pane.id);
      if (live) this.claimSession(live, session);
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
  }

  /** Note focus without asking for it, so a click does not re-trigger the focus effect. */
  noteFocus(worktreeId: string, paneId: string): void {
    this.focused = { ...this.focused, [worktreeId]: paneId };
    this.seen(paneId);
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
  }

  setRatio(worktreeId: string, path: string, ratio: number): void {
    const layout = this.layoutFor(worktreeId);
    if (!layout) return;
    this.layouts = { ...this.layouts, [worktreeId]: resize(layout, path, ratio) };
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
    this.panes = [...this.panes, side];

    try {
      const session = await commands.openAgentSideSession({
        parentSession: parent.session,
        options: { model: side.model, effort: side.effort, mode: side.mode },
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
    pane.approvals = pane.approvals.filter((a) => a.id !== requestId);
    try {
      await commands.answerApproval(pane.session, requestId, answer);
    } catch (e) {
      this.error = errorMessage(e);
    }
  }

  /**
   * End a session and drop its pane.
   *
   * The only thing that discards a transcript *and the pane with it* — `restart` also clears the
   * transcript, but keeps the pane and its position, and leaves the conversation resumable.
   */
  async close(paneId: string): Promise<void> {
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
    this.atCapacity = false;
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
    pane.approvals = [];
    pane.usage = null;
    // A fresh session has no turn in flight and nothing you have not seen. Both would otherwise
    // survive the reset and describe a process that no longer exists.
    pane.working = false;
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
              options: { model: pane.model, effort: pane.effort, mode: pane.mode },
            });
      this.claimSession(pane, session);
    } catch (e) {
      pane.error = errorMessage(e);
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
    this.atCapacity = false;
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
      for (const event of waiting) this.record(session, event);
    }
  }

  /** Hold an event for a session no pane owns yet. Oldest dropped first, as the transcript does. */
  private holdEvent(session: string, event: AgentEvent): void {
    const waiting = this.eventsAhead.get(session) ?? [];
    waiting.push(event);
    if (waiting.length > MAX_EARLY_EVENTS)
      waiting.splice(0, waiting.length - MAX_EARLY_EVENTS);
    this.eventsAhead.set(session, waiting);
  }

  private record(session: string, event: AgentEvent): void {
    const pane = this.paneBySession(session);
    if (!pane) {
      // Not noise to discard: a CLI that is not logged in says so on stderr during the handshake,
      // which is exactly the window where no pane owns the id yet. `session.rs` calls a silent
      // session with no transcript the worst possible presentation of that failure, and dropping
      // these was how it happened.
      this.holdEvent(session, event);
      return;
    }

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
      // A turn that starts is the only trustworthy evidence a limit has lifted, and it costs nothing
      // to read it that way — the alternative is a countdown against `resetsAt`, which needs a timer
      // and would clear the offer while the provider was still refusing.
      pane.limit = null;
    } else if (event.kind === 'turn_finished') {
      pane.working = false;
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
    } else if (event.kind === 'session_ready' && event.mode !== null) {
      // The one setting wtm can learn rather than choose. Claude passes no `--permission-mode`
      // precisely so `~/.claude/settings.json` decides, so without adopting the answer the mode
      // pill would show a default the session is not in.
      pane.mode = event.mode;
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

    pane.events.push(event);
    if (pane.events.length > MAX_EVENTS) {
      pane.events.splice(0, pane.events.length - MAX_EVENTS);
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
