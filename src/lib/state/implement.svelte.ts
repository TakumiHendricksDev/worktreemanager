/**
 * Implementing in the window: each opened issue's Implement tab.
 *
 * A store of its own, beside `plan` and `design`, because the tab and the inbox act on the same
 * run: a question answered from one is answered in the other.
 *
 * # Reading, never polling
 *
 * An issue's implementation is read when its Implement tab opens, after each action here, and on
 * `flow:changed` for a run it shows or for its review run: that is how a commit, a check or a fix
 * round arrives (ARCHITECTURE §8).
 */
import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import {
  errorMessage,
  type HandoffView,
  type ImplementView,
  type MergeMethod,
  type MergeSheet,
  type OwnPrView,
  type ReadySheet,
} from '../ipc/types';

/** How often, at most, the window's focus reads a pull request again. `FOCUS_GAP_MS` in Rust. */
const FOCUS_GAP_MS = 60_000;

class ImplementState {
  /** The project the views below belong to. */
  projectId = $state<string | null>(null);
  /** Issues' implementations, by key, as last read. `null` when the issue has no run. */
  views = $state<Record<string, ImplementView | null>>({});
  /** Keys with an action under way. */
  busy = $state<Record<string, boolean>>({});
  /** The last action's refusal, by key. */
  errors = $state<Record<string, string>>({});
  /** Hand-overs on screen, by key, as last read. */
  handoffs = $state<Record<string, HandoffView>>({});
  /** Issues' own pull requests on screen, by key, from the record. */
  pulls = $state<Record<string, OwnPrView>>({});
  /** When each issue's pull request was last read from GitHub by a focus, by key. */
  private focused: Record<string, number> = {};

  async init(): Promise<() => void> {
    return listen<{ flow: number }>('flow:changed', (event) => {
      const project = this.projectId;
      if (!project) return;
      const changed = event.payload.flow;
      for (const [key, view] of Object.entries(this.views)) {
        if (
          view === null ||
          view.flow.id === changed ||
          view.implementation?.reviewFlow === changed
        ) {
          void this.load(project, key);
          if (view && this.handoffs[key]) void this.loadHandoff(key, view.flow.id);
          if (view && this.pulls[key]) void this.loadPull(key, view.flow.id);
        }
      }
    });
  }

  private forProject(project: string): void {
    if (this.projectId === project) return;
    this.projectId = project;
    this.views = {};
    this.handoffs = {};
    this.pulls = {};
  }

  /** Read `key`'s implementation. */
  async load(project: string, key: string): Promise<void> {
    this.forProject(project);
    try {
      const view = await commands.issueImplementation(project, key);
      if (this.projectId !== project) return;
      this.views = { ...this.views, [key]: view };
    } catch (error) {
      if (this.projectId === project) this.fail(key, error);
    }
  }

  /**
   * Read `key`'s hand-over: its diff and evidence, read from the worktree, so only while the panel
   * is on screen and when its run changes.
   */
  async loadHandoff(key: string, flow: number): Promise<void> {
    const project = this.projectId;
    try {
      const view = await commands.implementationHandoff(flow);
      if (this.projectId !== project) return;
      this.handoffs = { ...this.handoffs, [key]: view };
    } catch (error) {
      if (this.projectId === project) this.fail(key, error);
    }
  }

  /** Read `key`'s own pull request from the record. Reads nothing remote. */
  async loadPull(key: string, flow: number): Promise<void> {
    const project = this.projectId;
    try {
      const view = await commands.issuePullRequest(flow);
      if (this.projectId !== project) return;
      this.pulls = { ...this.pulls, [key]: view };
    } catch (error) {
      if (this.projectId === project) this.fail(key, error);
    }
  }

  /** Stop following `key`'s pull request on screen. */
  forgetPull(key: string): void {
    this.pulls = Object.fromEntries(Object.entries(this.pulls).filter(([k]) => k !== key));
  }

  /**
   * Read `key`'s pull request from GitHub again: on demand, or because the window came back into
   * focus, which reads at most once a minute. Never on a timer.
   */
  async syncPull(key: string, flow: number, reason: 'demand' | 'focus'): Promise<void> {
    const now = Date.now();
    if (reason === 'focus' && now - (this.focused[key] ?? 0) < FOCUS_GAP_MS) return;
    this.focused[key] = now;
    const project = this.projectId;
    if (this.busy[key]) return;
    this.busy = { ...this.busy, [key]: true };
    this.clear(key);
    try {
      const view = await commands.syncPullRequest(flow);
      if (this.projectId === project) this.pulls = { ...this.pulls, [key]: view };
    } catch (error) {
      if (this.projectId === project) this.fail(key, error);
    } finally {
      this.busy = Object.fromEntries(Object.entries(this.busy).filter(([k]) => k !== key));
    }
  }

  /** The pull request on screen that a focus should read again, if any: one being followed. */
  shownPull(): { key: string; flow: number } | null {
    for (const [key, view] of Object.entries(this.pulls)) {
      if (view.canSync) return { key, flow: view.flow };
    }
    return null;
  }

  /** What Mark ready + Needs Review would do, read fresh; `null` with the refusal kept. */
  async readySheet(key: string, flow: number): Promise<ReadySheet | null> {
    this.clear(key);
    try {
      return await commands.readySheet(flow);
    } catch (error) {
      this.fail(key, error);
      return null;
    }
  }

  /** What the Merge sheet shows, read fresh; `null` with the refusal kept. */
  async mergeSheet(key: string, flow: number): Promise<MergeSheet | null> {
    this.clear(key);
    try {
      return await commands.mergeSheet(flow);
    } catch (error) {
      this.fail(key, error);
      return null;
    }
  }

  /** Stop reading `key`'s hand-over, as its panel leaves the screen. */
  forgetHandoff(key: string): void {
    this.handoffs = Object.fromEntries(
      Object.entries(this.handoffs).filter(([k]) => k !== key),
    );
  }

  private fail(key: string, error: unknown): void {
    this.errors = { ...this.errors, [key]: errorMessage(error) };
  }

  /** Clear `key`'s last refusal, as a new action does. */
  clear(key: string): void {
    this.errors = Object.fromEntries(
      Object.entries(this.errors).filter(([k]) => k !== key),
    );
  }

  private async act(
    project: string,
    key: string,
    action: () => Promise<void>,
  ): Promise<boolean> {
    this.forProject(project);
    if (this.busy[key]) return false;
    this.busy = { ...this.busy, [key]: true };
    this.clear(key);
    let ok = true;
    try {
      await action();
    } catch (error) {
      ok = false;
      this.fail(key, error);
    } finally {
      this.busy = Object.fromEntries(Object.entries(this.busy).filter(([k]) => k !== key));
      await this.load(project, key);
    }
    return ok;
  }

  start(project: string, key: string, flow: number) {
    return this.act(project, key, () => commands.startImplementing(flow));
  }

  resume(project: string, key: string, flow: number) {
    return this.act(project, key, () => commands.resumeImplementing(flow));
  }

  revise(project: string, key: string, flow: number, note: string) {
    return this.act(project, key, () => commands.reviseImplementation(flow, note));
  }

  reviseFromFeedback(project: string, key: string, flow: number) {
    return this.act(project, key, () => commands.reviseFromFeedback(flow));
  }

  approveReady(project: string, key: string, flow: number, head: string) {
    return this.act(project, key, () => commands.approveReady(flow, head));
  }

  approveMerge(
    project: string,
    key: string,
    flow: number,
    head: string,
    method: MergeMethod,
  ) {
    return this.act(project, key, () => commands.approveMerge(flow, head, method));
  }

  draftNow(project: string, key: string, flow: number) {
    return this.act(project, key, () => commands.draftPullRequest(flow));
  }

  retryWrite(project: string, key: string, approval: number) {
    return this.act(project, key, () => commands.retryPullRequestWrite(approval));
  }
}

export const implement = new ImplementState();
