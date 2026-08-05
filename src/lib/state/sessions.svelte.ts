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
  type AgentEvent,
  type AgentEventEnvelope,
  type AgentExit,
  type AgentOption,
  type AgentReady,
  type ApprovalAnswer,
  type ApprovalRequest,
  type Capability,
  type PtyExit,
  type Resumable,
} from '../ipc/types';
import {
  insert,
  panesOf,
  remove,
  resize,
  type Layout,
  type Placement,
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

export type SessionKind = { kind: 'shell' } | { kind: 'agent'; provider: string };

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
  /** Provider flags that are on, by name. Claude's `ultracode` is the only one today. */
  flags: string[];
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

  live = $derived(this.panes.filter((p) => p.ended === null && p.error === null));

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
    return this.panes.filter((p) => p.worktreeId === worktreeId);
  }

  layoutFor(worktreeId: string | null): Layout | null {
    if (worktreeId === null) return null;
    return this.layouts[worktreeId] ?? null;
  }

  /** The worktree's shell pane, if it has one. */
  shellIn(worktreeId: string | null): Pane | null {
    return this.panesIn(worktreeId).find((p) => p.kind.kind === 'shell') ?? null;
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
    });
    const offPtyExit = await listen<PtyExit>('pty:exit', (e) => {
      this.noteExit(e.payload.session, e.payload.summary);
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
    };
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

  /** Change what a pane's next turn uses. Takes effect without restarting the session. */
  configure(
    paneId: string,
    next: { model: string; effort: string; flags: string[] },
  ): void {
    const pane = this.paneById(paneId);
    if (!pane) return;
    pane.model = next.model;
    pane.effort = next.effort;
    pane.flags = next.flags;
  }

  /** What can be resumed in a worktree. Silent on failure, like every auxiliary read here. */
  async refreshResumable(worktreeId: string): Promise<void> {
    try {
      this.resumable = {
        ...this.resumable,
        [worktreeId]: await commands.listResumable(worktreeId),
      };
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
      if (live) live.session = session;
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

  /** Re-probe which agents this machine has. Silent on failure — an auxiliary convenience. */
  async refreshOptions(): Promise<void> {
    try {
      this.options = await commands.listAgents();
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
    pane.session = session;
    pane.ready = true;
    this.panes = [...this.panes, pane];
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
      ready: false,
      ended: null,
      error: null,
      generation: 0,
      model: null,
      effort: null,
      flags: [],
    };
  }

  private place(worktreeId: string, paneId: string, placement: Placement): void {
    this.layouts = {
      ...this.layouts,
      [worktreeId]: insert(
        this.layoutFor(worktreeId),
        paneId,
        this.focused[worktreeId] ?? null,
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
   * Open the worktree's shell, or focus it if it already has one.
   *
   * Idempotent per worktree, unlike an agent session: a second `$SHELL -l` in the same directory is
   * never what anyone means, which is also why `open_terminal` is idempotent in Rust.
   */
  async openShell(projectId: string, worktreeId: string): Promise<void> {
    const existing = this.shellIn(worktreeId);
    if (existing) {
      this.focus(worktreeId, existing.id);
      return;
    }
    if (!this.hasRoom(worktreeId)) return;

    const pane = this.blank({ kind: 'shell' }, projectId, worktreeId);
    // Appended before the spawn is asked for: the pane mounts a terminal with a null session, which
    // is what makes the shell's first prompt unlosable. See `Terminal.svelte`.
    this.panes = [...this.panes, pane];
    this.place(worktreeId, pane.id, 'below');

    try {
      const session = await commands.openTerminal({
        projectId,
        worktreeId,
        rows: SPAWN_ROWS,
        cols: SPAWN_COLS,
      });
      const live = this.paneById(pane.id);
      if (live) live.session = session;
    } catch (e) {
      const live = this.paneById(pane.id);
      if (live) live.error = errorMessage(e);
    }
  }

  /** Start an agent session. Not idempotent — several in one worktree is the feature. */
  async openAgent(
    projectId: string,
    worktreeId: string,
    provider: string,
    placement: Placement = 'right',
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

    this.panes = [...this.panes, pane];
    this.place(worktreeId, pane.id, placement);
    void this.loadCapability(provider);

    try {
      const session = await commands.openAgentSession({
        projectId,
        worktreeId,
        agentId: provider,
        options: { model: pane.model, effort: pane.effort },
      });
      const live = this.paneById(pane.id);
      if (live) live.session = session;
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
  }

  /** Note focus without asking for it, so a click does not re-trigger the focus effect. */
  noteFocus(worktreeId: string, paneId: string): void {
    this.focused = { ...this.focused, [worktreeId]: paneId };
  }

  setRatio(worktreeId: string, path: string, ratio: number): void {
    const layout = this.layoutFor(worktreeId);
    if (!layout) return;
    this.layouts = { ...this.layouts, [worktreeId]: resize(layout, path, ratio) };
  }

  async send(paneId: string, text: string): Promise<void> {
    const pane = this.paneById(paneId);
    if (!pane?.session) return;
    try {
      await commands.sendTurn(pane.session, text);
    } catch (e) {
      this.error = errorMessage(e);
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

  /** End a session and drop its pane. The only thing that discards a transcript. */
  async close(paneId: string): Promise<void> {
    const pane = this.paneById(paneId);
    if (!pane) return;

    if (pane.session) {
      try {
        if (pane.kind.kind === 'shell') await commands.closeTerminal(pane.worktreeId);
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
        if (pane.kind.kind === 'shell') await commands.closeTerminal(pane.worktreeId);
        else await commands.closeAgentSession(pane.session);
      } catch {
        /* Already gone is the ordinary case here. */
      }
    }

    pane.generation += 1;
    pane.session = null;
    pane.events = [];
    pane.approvals = [];
    pane.ready = false;
    pane.ended = null;
    pane.error = null;
    this.focus(pane.worktreeId, pane.id);

    try {
      if (pane.kind.kind === 'shell') {
        pane.session = await commands.openTerminal({
          projectId: pane.projectId,
          worktreeId: pane.worktreeId,
          rows: SPAWN_ROWS,
          cols: SPAWN_COLS,
        });
      } else {
        pane.session = await commands.openAgentSession({
          projectId: pane.projectId,
          worktreeId: pane.worktreeId,
          agentId: pane.kind.provider,
          options: { model: pane.model, effort: pane.effort },
        });
      }
    } catch (e) {
      pane.error = errorMessage(e);
    }
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
        void commands.closeTerminal(pane.worktreeId).catch(() => {});
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

  private record(session: string, event: AgentEvent): void {
    const pane = this.paneBySession(session);
    if (!pane) return;

    // Tracked outside the log as well as in it. The log is what the transcript renders; this is what
    // the pane is *blocked on*, and folding the whole log on every append would be O(events) per
    // delta on the hottest path in the app.
    if (event.kind === 'approval_requested') {
      pane.approvals = [...pane.approvals, { id: event.id, request: event.request }];
    } else if (event.kind === 'approval_resolved') {
      pane.approvals = pane.approvals.filter((a) => a.id !== event.id);
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
    // An approval nobody can answer any more would sit on screen forever.
    pane.approvals = [];
  }
}

export const sessions = new Sessions();
