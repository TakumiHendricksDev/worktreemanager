/**
 * New worktrees being made, and the record each leaves on the worktree it made.
 *
 * # Why this is not the New Worktree pane's own state
 *
 * It was, and that was the bug: the pane is a view, and a view unmounts when you look at another
 * worktree. The run carried on in Rust, but its steps, its command and its terminal lived in the
 * component, so leaving mid-setup lost all of it and coming back gave an empty form. Here a creation
 * outlives every screen that shows it. The sidebar draws each running one as a ghost tab where the
 * worktree will land; the pane shows whichever one is selected; and once the worktree exists, its
 * own view shows the record until it is dismissed.
 *
 * # Routed by id
 *
 * Every run is started with a creation id that Rust adds to each `wtm:progress` event it emits
 * (`ProgressBridge::tagged`), so two creations at once each get only their own steps. An event
 * without one belongs to nothing here.
 *
 * Not persisted. A creation is a call in flight and, after it, a note about how it went; after a
 * relaunch there is no run to follow and the worktree is simply a worktree.
 */
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { commands } from '../ipc/commands';
import {
  errorMessage,
  type CreateOutcome,
  type Preview,
  type ProgressEvent,
  type Worktree,
} from '../ipc/types';
import type { WorktreeIdentity } from '../worktree-label';
import { inPaneWindow } from '../window-role';
import { view } from './view.svelte';
import { workspace } from './workspace.svelte';

/**
 * Where a creation is.
 *
 * `refused` is a run that failed before git made anything — a branch that exists, a lookup that
 * failed — so there is no worktree and the form is what is worth going back to. The others after
 * `running` all have a worktree: it was made, its setup failed, or its setup was cancelled.
 */
export type CreationPhase =
  'running' | 'created' | 'setup_failed' | 'cancelled' | 'refused';

export interface CreationStep {
  id: string;
  label: string;
  index: number;
  total: number;
  /** Set once a later stage begins, which is what "this one finished" means here. */
  done: boolean;
}

export interface Creation {
  id: string;
  projectId: string;
  /** The form as it was submitted, kept to fill it in again after a refusal. */
  values: Record<string, string>;
  /** The submitted values as their fields normalized them, for `{{form.*}}`. */
  normalized: Record<string, string>;
  adoptBranch: string | null;
  acknowledged: string[];
  /** Where it will land, from the preview it was created from: the worktree's id is its path. */
  path: string;
  dirname: string;
  branch: string | null;
  issueKey: string | null;
  phase: CreationPhase;
  steps: CreationStep[];
  command: { argv: string[]; cwd: string } | null;
  notes: { id: string; text: string; kind: 'warning' | 'note' }[];
  /** The setup terminal, once it exists. */
  session: string | null;
  outcome: CreateOutcome | null;
  error: string | null;
  startedMs: number;
  finishedMs: number | null;
  /** Re-run setup is running on a worktree that already exists. */
  rerunning: boolean;
  /** The record has been dismissed from the worktree's view. */
  dismissed: boolean;
  /** "Not now" on the recipe question it carries. */
  offerDismissed: boolean;
}

/** What a creation's tab would be called, in the shape `worktreeLabel` reads. */
export function identityOf(creation: Creation): WorktreeIdentity {
  return {
    title: creation.dirname,
    dirname: creation.dirname,
    branch: creation.branch,
    path: creation.path,
    issueKey: creation.issueKey,
    isMain: false,
    head: null,
  };
}

/** The step the run is on, or null before the first. */
export function currentStep(creation: Creation): CreationStep | null {
  return creation.steps.find((s) => !s.done) ?? null;
}

/** The worktree a finished creation made, if it made one. */
export function madeWorktree(creation: Creation): Worktree | null {
  return creation.outcome?.worktree ?? null;
}

function basename(path: string): string {
  return path.replace(/\/+$/, '').split('/').pop() ?? path;
}

class Creations {
  list = $state<Creation[]>([]);
  /** The creation the New Worktree view shows, or null for a fresh form. */
  shown = $state<string | null>(null);

  async init(): Promise<UnlistenFn> {
    if (inPaneWindow) return () => {};
    return listen<ProgressEvent>('wtm:progress', (event) => {
      const id = event.payload.creation;
      if (id) this.progress(id, event.payload);
    });
  }

  byId(id: string | null): Creation | undefined {
    return id === null ? undefined : this.list.find((c) => c.id === id);
  }

  /** The creations still standing in for a worktree in a project's list: running or refused. */
  ghostsIn(projectId: string | null): Creation[] {
    return this.list.filter(
      (c) =>
        c.projectId === projectId &&
        (c.phase === 'running' ||
          c.phase === 'refused' ||
          (c.phase === 'cancelled' && !madeWorktree(c))),
    );
  }

  /**
   * Whether a real worktree's tab should stay out of the list for now.
   *
   * Git makes the worktree partway through a run — before setup, which can take minutes — and a
   * refresh in between would list it. Two tabs for one worktree, one of them saying it is finished,
   * is worse than the ghost alone, so the real one waits until the run is over.
   */
  hides(worktreeId: string): boolean {
    return this.list.some(
      (c) => c.phase === 'running' && !c.rerunning && c.path === worktreeId,
    );
  }

  /** The record a worktree's own view shows, if it has one that has not been dismissed. */
  recordFor(worktreeId: string | null): Creation | undefined {
    if (worktreeId === null) return undefined;
    return this.list.find(
      (c) => !c.dismissed && c.phase !== 'refused' && madeWorktree(c)?.id === worktreeId,
    );
  }

  /** What a worktree's tab says about having just been made: the `fresh` prop of WorktreeTab. */
  freshness(worktreeId: string): 'created' | 'setup_failed' | 'setting_up' | null {
    const record = this.recordFor(worktreeId);
    if (!record) return null;
    if (record.rerunning) return 'setting_up';
    if (record.phase === 'setup_failed' || record.phase === 'cancelled')
      return 'setup_failed';
    return record.phase === 'created' ? 'created' : null;
  }

  /** Open the New Worktree view on a blank form. */
  showForm(): void {
    this.shown = null;
    view.show('new');
  }

  /** Open the New Worktree view on a creation: its progress, or its form if it was refused. */
  show(id: string): void {
    this.shown = id;
    view.show('new');
  }

  /**
   * Start making a worktree, and return its id.
   *
   * `retry` is a refused creation being tried again from its own form: it keeps its id and its
   * place in the list rather than becoming a second ghost beside the first.
   */
  start(args: {
    projectId: string;
    values: Record<string, string>;
    adoptBranch: string | null;
    acknowledged: string[];
    preview: Preview;
    retry?: string | null;
  }): string {
    const id = args.retry ?? crypto.randomUUID();
    const creation: Creation = {
      id,
      projectId: args.projectId,
      values: { ...args.values },
      normalized: { ...args.preview.normalized },
      adoptBranch: args.adoptBranch,
      acknowledged: [...args.acknowledged],
      path: args.preview.directory,
      dirname: basename(args.preview.directory),
      branch: args.preview.branch,
      issueKey: args.preview.issueKey,
      phase: 'running',
      steps: [],
      command: null,
      notes: [],
      session: null,
      outcome: null,
      error: null,
      startedMs: Date.now(),
      finishedMs: null,
      rerunning: false,
      dismissed: false,
      offerDismissed: false,
    };
    const at = this.list.findIndex((c) => c.id === id);
    this.list =
      at === -1
        ? [...this.list, creation]
        : this.list.map((c, i) => (i === at ? creation : c));
    void this.run(id);
    return id;
  }

  private async run(id: string): Promise<void> {
    const creation = this.byId(id);
    if (!creation) return;
    try {
      const result = await commands.createWorktree({
        projectId: creation.projectId,
        values: { ...creation.values },
        adoptBranch: creation.adoptBranch,
        acknowledged: [...creation.acknowledged],
        rows: 24,
        cols: 100,
        creation: id,
      });
      this.update(id, (c) => {
        c.outcome = result;
        c.finishedMs = Date.now();
        // Normally `session_started` set this mid-run; these cover a run that produced no session
        // event at all — no setup command, or a spawn that failed.
        if (result.kind === 'created') c.session ??= result.setupSession;
        else c.session ??= result.session;
        c.phase =
          result.kind === 'created'
            ? 'created'
            : result.kind === 'setup_failed'
              ? 'setup_failed'
              : 'cancelled';
      });
      await workspace.refreshWorktrees();
      this.arrive(id);
    } catch (e) {
      this.update(id, (c) => {
        c.phase = 'refused';
        c.error = errorMessage(e);
        c.finishedMs = Date.now();
      });
    }
  }

  /**
   * A run is over and its worktree exists: if its screen is the one on display, go to the
   * worktree, whose own view now carries the record.
   *
   * Only then. Someone who started a creation and went to work elsewhere is not pulled back to it
   * when it finishes; its tab saying "created" is how they find out.
   */
  private arrive(id: string): void {
    const creation = this.byId(id);
    const made = creation ? madeWorktree(creation) : null;
    if (!creation || !made) return;
    if (this.shown === id && view.current === 'new') {
      this.shown = null;
      if (workspace.activeProjectId === creation.projectId) workspace.select(made.id);
      view.show('worktree');
    }
  }

  /** Run setup again on a worktree whose setup failed or was cancelled. */
  async rerun(id: string): Promise<void> {
    const creation = this.byId(id);
    const worktree = creation ? madeWorktree(creation) : null;
    if (!creation || !worktree || creation.rerunning) return;
    this.update(id, (c) => {
      c.rerunning = true;
      c.offerDismissed = false;
      c.error = null;
      c.notes = [];
      c.command = null;
      c.session = null;
      c.steps = [];
    });
    try {
      const result = await commands.runSetup({
        projectId: creation.projectId,
        worktreeId: worktree.id,
        extraArgs: [],
        rows: 24,
        cols: 100,
        creation: id,
      });
      this.update(id, (c) => {
        c.session ??= result.session;
        c.finishedMs = Date.now();
        if (result.success) {
          c.phase = 'created';
          c.outcome = { kind: 'created', worktree, setupSession: result.session };
        } else {
          c.error = result.summary;
        }
      });
      await workspace.refreshWorktrees();
    } catch (e) {
      this.update(id, (c) => {
        c.error = errorMessage(e);
      });
    } finally {
      this.update(id, (c) => {
        c.rerunning = false;
      });
    }
  }

  /** Throw away an attempt that made nothing. */
  discard(id: string): void {
    this.list = this.list.filter((c) => c.id !== id);
    if (this.shown === id) {
      this.shown = null;
      if (view.current === 'new') view.show(view.previous === 'home' ? 'home' : 'worktree');
    }
  }

  /** Hide a worktree's record. The worktree stays; only the note about how it was made goes. */
  dismiss(id: string): void {
    this.update(id, (c) => {
      c.dismissed = true;
    });
  }

  dismissOffer(id: string): void {
    this.update(id, (c) => {
      c.offerDismissed = true;
    });
  }

  /** A worktree that is gone takes its record with it. */
  forget(worktreeId: string): void {
    this.list = this.list.filter((c) => madeWorktree(c)?.id !== worktreeId);
  }

  private update(id: string, change: (c: Creation) => void): void {
    const creation = this.list.find((c) => c.id === id);
    if (creation) change(creation);
  }

  private progress(id: string, payload: ProgressEvent): void {
    this.update(id, (c) => {
      switch (payload.kind) {
        case 'stage':
          // A stage beginning is the signal that every earlier one is over; the pipeline does not
          // emit a separate "finished", and inferring it here beats adding one.
          c.steps = [
            ...c.steps
              .filter((s) => s.id !== payload.id)
              .map((s) => ({ ...s, done: true })),
            {
              id: payload.id,
              label: payload.label,
              index: payload.index,
              total: payload.total,
              done: payload.id === 'done',
            },
          ];
          c.command = null;
          break;
        case 'command_started':
          c.command = { argv: payload.argv, cwd: payload.cwd };
          break;
        case 'command_finished':
          c.command = null;
          break;
        case 'session_started':
          c.session = payload.session;
          break;
        case 'warning':
          c.notes = [
            ...c.notes,
            {
              id: `warn-${c.notes.length}-${payload.id}`,
              text: payload.message,
              kind: 'warning',
            },
          ];
          break;
        case 'note':
          c.notes = [
            ...c.notes,
            { id: `note-${c.notes.length}`, text: payload.message, kind: 'note' },
          ];
          break;
        default:
          break;
      }
    });
  }
}

export const creations = new Creations();
