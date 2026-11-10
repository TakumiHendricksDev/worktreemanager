/**
 * What each integrated native CLI session reports, keyed by its backend session id.
 *
 * Rust owns the facts and announces each change as `native:changed`. A snapshot carries a sequence
 * number, so one that arrives late — a reply to `native_session` overtaken by an event — cannot
 * put an older state back.
 */
import { listen } from '@tauri-apps/api/event';
import { commands } from '../ipc/commands';
import type {
  NativeDeliveryCheck,
  NativeReceipt,
  NativeResolution,
  NativeSession,
} from '../native-sessions';

class NativeSessions {
  states = $state<Record<string, NativeSession>>({});

  /**
   * Which Open Design review each pane last revealed by itself, by pane id.
   *
   * Outside the component on purpose. A pane's view is remounted by every Reconnect, Restart and
   * put-back, and a remembered-in-component review is one that springs open again each time,
   * covering the terminal the user had just chosen to look at. Not reactive: only the decision
   * to reveal reads it, never the markup.
   */
  private revealed = new Map<string, string>();

  /**
   * Sessions a pane has let go of, by closing or restarting. An announcement already on its way
   * when that happened would otherwise put back a state no pane will ever read.
   */
  private gone = new Set<string>();

  /**
   * Keep a snapshot unless a newer one is already held.
   *
   * One key is written rather than the map replaced: `states` is read by every native pane, the
   * header status of each and the sidebar's dots, and a new map would make all of them recompute
   * for an activity update in one of them.
   */
  accept(state: NativeSession) {
    if (this.gone.has(state.session)) return false;
    if ((this.states[state.session]?.seq ?? -1) > state.seq) return false;
    this.states[state.session] = state;
    return true;
  }

  /** Drop a session that is gone, so a closed pane's last state is not kept for the window's life. */
  forget(session: string | null) {
    if (session === null) return;
    this.gone.add(session);
    if (session in this.states) delete this.states[session];
  }

  async load(session: string) {
    const state = await commands.nativeSession(session);
    this.accept(state);
    return this.states[session] ?? state;
  }

  /**
   * Check delivery: ask the provider's history about this session's uncertain receipts.
   *
   * The answer is returned whole, because what the button says afterwards is the point: Claude
   * has no history WTM can read, a disconnected session cannot be asked, and a Codex history that
   * does not show an instruction leaves it uncertain rather than proving it lost.
   */
  async check(session: string): Promise<NativeDeliveryCheck> {
    const check = await commands.checkNativeDelivery(session);
    this.accept(check.session);
    return check;
  }

  /**
   * The user's answer for a receipt no evidence will settle. Nothing is resent either way.
   *
   * Reloads afterwards, success or not: a refusal means the receipt changed under the user, and
   * showing the state that refused them is what lets them see why.
   */
  async resolve(
    session: string,
    receipt: string,
    resolution: NativeResolution,
  ): Promise<NativeReceipt> {
    try {
      return await commands.resolveNativeDelivery(session, receipt, resolution);
    } finally {
      await this.load(session).catch(() => {});
    }
  }

  /** Whether a pane should open its review panel for `review`, and remember that it has. */
  revealOnce(paneId: string, review: string): boolean {
    if (this.revealed.get(paneId) === review) return false;
    this.revealed.set(paneId, review);
    return true;
  }

  forgetPane(paneId: string) {
    this.revealed.delete(paneId);
  }

  async init(changed: (state: NativeSession) => void) {
    const off = await listen<NativeSession>('native:changed', ({ payload }) => {
      if (this.accept(payload)) changed(payload);
    });
    const states = await commands.nativeSessions().catch(() => []);
    for (const state of states) {
      if (this.accept(state)) changed(state);
    }
    return off;
  }
}

export const nativeSessions = new NativeSessions();
