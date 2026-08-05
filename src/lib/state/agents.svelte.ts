/**
 * Agent sessions: which exist, what they have said, and what they are waiting on.
 *
 * # Why this is its own store and not part of `terminals`
 *
 * It will not be, for long. The plan is one session system where a shell is a `kind` alongside an
 * agent, and this file is the deliberate first half of that: the walking skeleton needs somewhere
 * to keep a transcript before the pane container it will eventually live in exists. Merging the
 * two while neither is settled would mean rewriting the merged thing twice.
 *
 * # Three rules, borrowed from `terminals` because they were right there
 *
 * 1. **A pane outlives the view.** Sessions live in one list here and nothing about the UI removes
 *    them — not switching worktrees, not switching projects. A transcript you scrolled an hour ago
 *    is still there when you come back.
 * 2. **Rust owns the session.** This side holds an id and a list of events. It never decides that
 *    a session has ended; `agent:exit` does.
 * 3. **No DOM.** Focus is not state.
 *
 * # Why the transcript is a flat event list rather than a message tree
 *
 * Both CLIs stream deltas, so a "message" is not a thing that arrives — it is a run of deltas that
 * has to be coalesced. Doing that at render time from a flat log keeps this store dumb and keeps
 * the coalescing rule in one place, where it can be changed without migrating stored state. It
 * also means an unrecognised `raw` event costs nothing: it is one more row.
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
} from '../ipc/types';

/**
 * How many events one session's transcript keeps.
 *
 * A bound is unavoidable — a long session with a chatty tool streams tens of thousands of deltas,
 * and an unbounded array in a `$state` proxy is a leak with a rendering cost attached. Overflow
 * drops the oldest, which degrades to "the top of a very long transcript is gone" rather than to
 * an unresponsive window. Deliberately generous: this is roughly an hour of hard use.
 */
const MAX_EVENTS = 20_000;

/** An approval this session is blocked on. */
export interface PendingApproval {
  id: string;
  request: ApprovalRequest;
}

export interface AgentPane {
  /** Backend session id. Null between asking for a session and being told its id. */
  session: string | null;
  projectId: string;
  worktreeId: string;
  provider: string;
  /** Everything the session has said, oldest first. */
  events: AgentEvent[];
  /** False until `agent:ready`. The composer stays enabled — turns are queued, not refused. */
  ready: boolean;
  /**
   * Approvals awaiting an answer, oldest first.
   *
   * A list rather than a single slot: the server can have more than one outstanding at once — a
   * command and a file change in the same turn — and dropping the second would stall the session
   * with no card to explain it. Rendered one at a time, oldest first, because answering them in
   * arrival order is the only order the transcript above makes sense in.
   */
  approvals: PendingApproval[];
  /** How the session ended, once it has. The pane stays so the transcript stays readable. */
  ended: string | null;
  /** A spawn that failed. Distinct from `ended`: there was never a session. */
  error: string | null;
  /**
   * Bumped by a restart, and part of the key the surface renders with, so a restart remounts.
   *
   * Same reasoning as the terminal dock's: continuing in place would put a fresh transcript
   * directly under a dead one with nothing to distinguish them.
   */
  generation: number;
}

class Agents {
  panes = $state<AgentPane[]>([]);
  /** The catalogue, with availability. Re-probed on open so a fresh install shows up. */
  options = $state<AgentOption[]>([]);
  error = $state<string | null>(null);

  /** Sessions still running. */
  live = $derived(this.panes.filter((p) => p.ended === null && p.error === null));

  paneFor(session: string | null): AgentPane | null {
    if (session === null) return null;
    return this.panes.find((p) => p.session === session) ?? null;
  }

  panesIn(worktreeId: string | null): AgentPane[] {
    if (worktreeId === null) return [];
    return this.panes.filter((p) => p.worktreeId === worktreeId);
  }

  /**
   * Subscribe to the three event streams, and adopt sessions that outlived a reload.
   *
   * Adopting matters even though an adopted pane comes back with an empty transcript — Rust
   * buffers nothing, so there is no history to restore. What it prevents is a session that is
   * running with nothing able to reach it: without this, a reload during `just dev` leaks a CLI
   * per pane for the life of the process.
   */
  async init(): Promise<UnlistenFn> {
    const offEvent = await listen<AgentEventEnvelope>('agent:event', (e) => {
      this.record(e.payload.session, e.payload.event);
    });
    const offExit = await listen<AgentExit>('agent:exit', (e) => {
      this.noteExit(e.payload.session, e.payload.summary);
    });
    const offReady = await listen<AgentReady>('agent:ready', (e) => {
      const pane = this.paneFor(e.payload.session);
      if (pane) pane.ready = true;
    });

    void this.refreshOptions();

    const running = await commands.listAgentSessions().catch(() => []);
    if (running.length > 0) {
      this.panes = running.map((s) => ({
        session: s.session,
        projectId: s.project,
        worktreeId: s.worktree,
        provider: s.provider,
        events: [],
        ready: true,
        approvals: [],
        ended: null,
        error: null,
        generation: 0,
      }));
    }

    return () => {
      offEvent();
      offExit();
      offReady();
    };
  }

  /**
   * Re-probe which agents this machine has.
   *
   * Silent on failure, and the reason is the same one `workspace.refreshOpeners` gives: this is
   * an auxiliary convenience, and surfacing "could not list agents" in the banner reserved for
   * git and config errors would be noise.
   */
  async refreshOptions(): Promise<void> {
    try {
      this.options = await commands.listAgents();
    } catch {
      /* Deliberately silent. See above. */
    }
  }

  /** Start a session in a worktree. Appends the pane before the spawn, so nothing is lost. */
  async open(projectId: string, worktreeId: string, provider: string): Promise<void> {
    // Appended before the spawn is even asked for, deliberately: events for this session can
    // arrive before the command returns its id, and a pane that does not exist yet cannot
    // record them. `record` tolerates that by matching on the session id once it is known.
    const pane: AgentPane = {
      session: null,
      projectId,
      worktreeId,
      provider,
      events: [],
      ready: false,
      approvals: [],
      ended: null,
      error: null,
      generation: 0,
    };
    this.panes = [...this.panes, pane];

    // Found by identity rather than by id, because the id is what we are about to learn. `$state`
    // proxies on assignment, so the reference above is not the object the list holds — see the
    // same note in `terminals.spawn`.
    const index = this.panes.length - 1;
    try {
      const session = await commands.openAgentSession({
        projectId,
        worktreeId,
        agentId: provider,
      });
      const live = this.panes[index];
      if (live) live.session = session;
      this.error = null;
    } catch (e) {
      const live = this.panes[index];
      if (live) live.error = errorMessage(e);
    }
  }

  /** Send a turn. */
  async send(session: string, text: string): Promise<void> {
    try {
      await commands.sendTurn(session, text);
    } catch (e) {
      this.error = errorMessage(e);
    }
  }

  async interrupt(session: string): Promise<void> {
    try {
      await commands.interruptTurn(session);
    } catch (e) {
      this.error = errorMessage(e);
    }
  }

  /** End a session and drop its pane. The only thing that discards a transcript. */
  async close(session: string): Promise<void> {
    try {
      await commands.closeAgentSession(session);
    } catch (e) {
      this.error = errorMessage(e);
    }
    this.panes = this.panes.filter((p) => p.session !== session);
  }

  /** Append one event to a session's transcript. */
  private record(session: string, event: AgentEvent): void {
    const pane = this.paneFor(session);
    if (!pane) return;

    // Tracked outside the event log as well as in it. The log is what the transcript renders; this
    // is what the pane is *blocked on*, and deriving it by folding the whole log on every append
    // would be O(events) per delta on the hottest path in the app.
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

  /**
   * Answer an approval.
   *
   * The card is removed locally as well as by the `approval_resolved` event that follows, so it
   * disappears on click rather than after a round trip. Safe for the same reason `toggleFavorite`
   * is optimistic: nothing else has an opinion about whether this card is still open, and the
   * authoritative removal arrives moments later and agrees.
   */
  async answer(session: string, requestId: string, answer: ApprovalAnswer): Promise<void> {
    const pane = this.paneFor(session);
    if (pane) pane.approvals = pane.approvals.filter((a) => a.id !== requestId);
    try {
      await commands.answerApproval(session, requestId, answer);
    } catch (e) {
      this.error = errorMessage(e);
    }
  }

  private noteExit(session: string, summary: string): void {
    const pane = this.paneFor(session);
    if (!pane) return;
    pane.ended = summary;
    pane.ready = false;
  }
}

export const agents = new Agents();
