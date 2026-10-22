/**
 * Start Work in the window: each opened issue's start, run and moves, the Start sheet, and the
 * actions that reach the engine.
 *
 * A store of its own, beside `trackerQueue`, for the reason that one gives: the queue is the
 * tracker's reading, and this is wtm's record of what it is doing about an issue. Both the queue
 * column and the main column act through it — the row's ⌘↵, the workspace's Start, a card dropped
 * on the board — so the sheet that asks where the work should happen, or whether an issue may
 * leave review, is one sheet whichever of them asked.
 *
 * # Reading, never polling
 *
 * An issue's view is read when its workspace opens, after each action here, and again on
 * `flow:changed` for a run it shows, which is how a worktree being made or a move being sent
 * arrives (ARCHITECTURE §8). After an action the queue is synced (`after_action`), so the team
 * status the tracker now has is what the board shows.
 */
import { listen } from '@tauri-apps/api/event';

import { commands } from '../ipc/commands';
import { errorMessage } from '../ipc/types';
import type { DropOutcome, IssueStartView, MoveAction, StartPreview } from '../start-work';
import { trackerQueue } from './tracker.svelte';
import { work } from './work.svelte';
import { workspace } from './workspace.svelte';

/** What the Start sheet is asking. */
export type Sheet =
  | {
      kind: 'choose';
      project: string;
      preview: StartPreview;
      byDrop: boolean;
    }
  | {
      kind: 'confirm';
      project: string;
      key: string;
      from: string;
      to: string;
      /** What confirming does: start the issue, or move it to `to`. */
      then: 'start' | 'move';
      byDrop: boolean;
      choice: string | null;
    };

class StartWorkState {
  /** The project the views below belong to. */
  projectId = $state<string | null>(null);
  /** Issues' starts, by key, as last read. */
  views = $state<Record<string, IssueStartView>>({});
  /** Keys with an action under way. */
  busy = $state<Record<string, boolean>>({});
  /** The last action's refusal, by key. */
  errors = $state<Record<string, string>>({});
  /** A drop the board could not make, for its header. */
  boardError = $state<string | null>(null);
  sheet = $state<Sheet | null>(null);

  async init(): Promise<() => void> {
    const off = await listen<{ flow: number }>('flow:changed', (event) => {
      const project = this.projectId;
      if (!project) return;
      for (const [key, view] of Object.entries(this.views)) {
        if (view.flow?.id === event.payload.flow || view.job?.flow === event.payload.flow)
          void this.load(project, key);
      }
    });
    return off;
  }

  private forProject(project: string): void {
    if (this.projectId === project) return;
    this.projectId = project;
    this.views = {};
    this.busy = {};
    this.errors = {};
    this.boardError = null;
    this.sheet = null;
  }

  /**
   * Read `key`'s start, run and moves, and what that changed elsewhere: a worktree the engine has
   * just made is not in the window's list until it is listed again, and a move it has just made is
   * in the mirror's whole read of the issue, which the workspace's header shows.
   */
  async load(project: string, key: string): Promise<void> {
    this.forProject(project);
    try {
      const before = this.views[key];
      const view = await commands.issueStart(project, key);
      if (this.projectId !== project) return;
      this.views = { ...this.views, [key]: view };
      const job = view.job;
      const made =
        job !== null &&
        job.state !== 'creating' &&
        workspace.activeProjectId === project &&
        !workspace.worktrees.some((w) => w.id === job.worktree);
      if (made) void workspace.refreshWorktrees();
      const moved = view.moves.some(
        (m) =>
          m.state === 'published' &&
          !before?.moves.some((b) => b.approval === m.approval && b.state === 'published'),
      );
      if (moved) void trackerQueue.kept(project, key);
    } catch (error) {
      if (this.projectId === project)
        this.errors = { ...this.errors, [key]: errorMessage(error) };
    }
  }

  private async act(
    project: string,
    key: string,
    action: () => Promise<void>,
  ): Promise<void> {
    this.forProject(project);
    if (this.busy[key]) return;
    this.busy = { ...this.busy, [key]: true };
    this.errors = Object.fromEntries(
      Object.entries(this.errors).filter(([k]) => k !== key),
    );
    try {
      await action();
    } catch (error) {
      if (this.projectId === project)
        this.errors = { ...this.errors, [key]: errorMessage(error) };
    } finally {
      this.busy = Object.fromEntries(Object.entries(this.busy).filter(([k]) => k !== key));
      if (this.projectId === project) {
        await this.load(project, key);
        void trackerQueue.sync(project, 'after_action');
      }
    }
  }

  /**
   * Start work on `key`, from the button, ⌘↵ or a drop on the start column. A run under way is
   * focused; a choice or a confirmation opens the sheet, and nothing is recorded until it is
   * answered.
   */
  start(
    project: string,
    key: string,
    options: { byDrop?: boolean; choice?: string | null; confirmed?: boolean } = {},
  ): Promise<void> {
    const byDrop = options.byDrop ?? false;
    const choice = options.choice ?? null;
    return this.act(project, key, async () => {
      const outcome = await commands.startWork({
        project,
        key,
        choice,
        byDrop,
        confirmed: options.confirmed ?? false,
      });
      switch (outcome.kind) {
        case 'started':
        case 'focused':
          this.sheet = null;
          // Focus is the issue's workspace, where its run is: from the board too.
          work.pickIssue(key);
          break;
        case 'choose':
          this.sheet = { kind: 'choose', project, preview: outcome.preview, byDrop };
          break;
        case 'confirm':
          this.sheet = {
            kind: 'confirm',
            project,
            key,
            from: outcome.from,
            to: outcome.to,
            then: 'start',
            byDrop,
            choice,
          };
          break;
      }
    });
  }

  /** A card dropped on column `to`. The start column is a Start; any other a status change. */
  async drop(project: string, key: string, to: string): Promise<void> {
    this.forProject(project);
    this.boardError = null;
    if (this.busy[key]) return;
    this.busy = { ...this.busy, [key]: true };
    let outcome: DropOutcome | null = null;
    try {
      outcome = await commands.moveIssue(project, key, to, false);
    } catch (error) {
      if (this.projectId === project)
        this.boardError = `${key} was not moved: ${errorMessage(error)}`;
    } finally {
      this.busy = Object.fromEntries(Object.entries(this.busy).filter(([k]) => k !== key));
    }
    if (this.projectId !== project) return;
    switch (outcome?.kind) {
      case 'start':
        return this.start(project, key, { byDrop: true });
      case 'confirm':
        this.sheet = {
          kind: 'confirm',
          project,
          key,
          from: outcome.from,
          to: outcome.to,
          then: 'move',
          byDrop: true,
          choice: null,
        };
        return;
      case 'moving':
        await this.load(project, key);
        void trackerQueue.sync(project, 'after_action');
        return;
      case 'stayed':
      case undefined:
        return;
    }
  }

  /** Answer the sheet's confirmation. */
  confirm(): Promise<void> {
    const sheet = this.sheet;
    if (sheet?.kind !== 'confirm') return Promise.resolve();
    this.sheet = null;
    if (sheet.then === 'start')
      return this.start(sheet.project, sheet.key, {
        byDrop: sheet.byDrop,
        choice: sheet.choice,
        confirmed: true,
      });
    return this.act(sheet.project, sheet.key, async () => {
      await commands.moveIssue(sheet.project, sheet.key, sheet.to, true);
    });
  }

  /** Answer the sheet's choice of where the work happens. */
  choose(choice: string): Promise<void> {
    const sheet = this.sheet;
    if (sheet?.kind !== 'choose') return Promise.resolve();
    this.sheet = null;
    return this.start(sheet.project, sheet.preview.key, { byDrop: sheet.byDrop, choice });
  }

  resume(project: string, key: string, flow: number, asIs: boolean): Promise<void> {
    return this.act(project, key, () => commands.resumeStart(flow, asIs));
  }

  cancel(project: string, key: string, flow: number): Promise<void> {
    return this.act(project, key, () => commands.cancelIssueRun(flow));
  }

  pause(project: string, key: string, flow: number): Promise<void> {
    return this.act(project, key, () => commands.pauseRun(flow));
  }

  unpause(project: string, key: string, flow: number): Promise<void> {
    return this.act(project, key, () => commands.unpauseRun(flow));
  }

  settleMove(
    project: string,
    key: string,
    approval: number,
    action: MoveAction,
  ): Promise<void> {
    return this.act(project, key, () => commands.settleMove(approval, action));
  }
}

export const startWork = new StartWorkState();
