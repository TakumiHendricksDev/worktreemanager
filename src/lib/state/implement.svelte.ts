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
import { errorMessage, type HandoffView, type ImplementView } from '../ipc/types';

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
        }
      }
    });
  }

  private forProject(project: string): void {
    if (this.projectId === project) return;
    this.projectId = project;
    this.views = {};
    this.handoffs = {};
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
}

export const implement = new ImplementState();
