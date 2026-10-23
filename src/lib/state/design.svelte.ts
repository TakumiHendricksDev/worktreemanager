/**
 * Designing in the window: each opened issue's Design tab.
 *
 * A store of its own, beside `plan`, because the Design tab and the inbox's design-ready card act on
 * the same run: a revision approved from one is approved in the other.
 *
 * # Reading, never polling
 *
 * An issue's design is read when its Design tab opens, after each action here, and on
 * `flow:changed` for a run it shows: that is how a published revision arrives (ARCHITECTURE §8).
 * Whether an approval still covers its sources is Rust's to say, when it reads them.
 */
import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import { errorMessage, type BrowserView, type DesignView } from '../ipc/types';

class DesignState {
  /** The project the views below belong to. */
  projectId = $state<string | null>(null);
  /** Issues' designs, by key, as last read. `null` when the issue has no run. */
  views = $state<Record<string, DesignView | null>>({});
  /** Keys with an action under way. */
  busy = $state<Record<string, boolean>>({});
  /** The last action's refusal, by key. */
  errors = $state<Record<string, string>>({});

  async init(): Promise<() => void> {
    return listen<{ flow: number }>('flow:changed', (event) => {
      const project = this.projectId;
      if (!project) return;
      for (const [key, view] of Object.entries(this.views)) {
        if (view === null || view.flow.id === event.payload.flow)
          void this.load(project, key);
      }
    });
  }

  private forProject(project: string): void {
    if (this.projectId === project) return;
    this.projectId = project;
    this.views = {};
  }

  /** Read `key`'s design. */
  async load(project: string, key: string): Promise<void> {
    this.forProject(project);
    try {
      const view = await commands.issueDesign(project, key);
      if (this.projectId !== project) return;
      this.views = { ...this.views, [key]: view };
    } catch (error) {
      if (this.projectId === project) this.fail(key, error);
    }
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

  request(project: string, key: string, flow: number, words: string) {
    return this.act(project, key, () => commands.requestDesign(flow, words));
  }

  start(project: string, key: string, flow: number) {
    return this.act(project, key, () => commands.startDesigner(flow));
  }

  comment(project: string, key: string, flow: number, revision: number, text: string) {
    return this.act(project, key, async () => {
      await commands.commentOnDesign(flow, revision, text);
    });
  }

  approve(
    project: string,
    key: string,
    flow: number,
    revision: number,
    fingerprint: string,
  ) {
    return this.act(project, key, async () => {
      await commands.approveDesign(flow, revision, fingerprint);
    });
  }

  requestChanges(
    project: string,
    key: string,
    flow: number,
    revision: number,
    note: string,
  ) {
    return this.act(project, key, async () => {
      await commands.requestDesignChanges(flow, revision, note);
    });
  }

  implement(project: string, key: string, flow: number) {
    return this.act(project, key, () => commands.startImplementing(flow));
  }

  /**
   * Open a revision in the sandboxed review pane. The pane is a tile in the issue's worktree, so
   * the caller shows that worktree once it is open.
   */
  async preview(
    project: string,
    key: string,
    flow: number,
    revision: number,
  ): Promise<BrowserView | null> {
    let opened: BrowserView | null = null;
    await this.act(project, key, async () => {
      opened = await commands.previewDesignRevision(flow, revision);
    });
    return opened;
  }
}

export const design = new DesignState();
