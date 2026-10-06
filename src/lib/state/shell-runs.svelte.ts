/** Ephemeral approvals survive a frontend reload, but never an app restart. */
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { commands } from '../ipc/commands';
import { errorMessage } from '../ipc/types';
import { isHome } from '../home';
import { inPaneWindow, toMain } from '../window-role';
import { runSettled, type ShellRun, type ShellSnippet } from '../shell-script';
import { sessions } from './sessions.svelte';
import { attention } from './attention.svelte';

export interface ShellReview {
  id: string;
  projectId: string;
  worktreeId: string;
  snippet: ShellSnippet;
  run?: ShellRun;
}
class ShellRuns {
  review = $state<ShellReview | null>(null);
  runs = $state<ShellRun[]>([]);
  private admitting = new Set<string>();
  private noticed = new Set<string>();

  open(projectId: string, worktreeId: string, snippet: ShellSnippet): void {
    if (inPaneWindow) {
      toMain({ kind: 'reviewShell', projectId, worktreeId, snippet });
      return;
    }
    this.review = {
      id: crypto.randomUUID(),
      projectId: isHome(worktreeId) ? '' : projectId,
      worktreeId: isHome(worktreeId) ? '' : worktreeId,
      snippet: { ...snippet },
    };
  }
  reviewHome(run: ShellRun): void {
    this.review = {
      id: run.id,
      projectId: run.request.projectId,
      worktreeId: run.request.worktreeId,
      snippet: run.request,
      run,
    };
  }
  upsert(run: ShellRun): void {
    const old = this.runs.find((item) => item.id === run.id);
    const phases = [
      'prepared',
      'awaiting_approval',
      'awaiting_pane',
      'opening_shell',
      'reserved',
      'running',
    ];
    if (
      old &&
      (runSettled(old) ||
        (!runSettled(run) && phases.indexOf(old.phase) > phases.indexOf(run.phase)))
    )
      return;
    this.runs = [...this.runs.filter((old) => old.id !== run.id), run];
    if (run.phase === 'awaiting_approval' && !this.noticed.has(run.id)) {
      this.noticed.add(run.id);
      attention.notice(
        'Home command needs you',
        'Review it in Home’s Needs you list. Nothing has run.',
      );
    }
  }
  async admit(run: ShellRun): Promise<void> {
    if (inPaneWindow || run.phase !== 'awaiting_pane' || this.admitting.has(run.id)) return;
    this.admitting.add(run.id);
    try {
      await sessions.admitShellRun(run);
    } catch (e) {
      attention.notice('Shell did not open', errorMessage(e));
    } finally {
      this.admitting.delete(run.id);
    }
  }
  async init(): Promise<UnlistenFn> {
    const changed = await listen<ShellRun>('shell-run:changed', ({ payload }) =>
      this.upsert(payload),
    );
    const pane = await listen<ShellRun>('shell-run:needs-pane', ({ payload }) => {
      this.upsert(payload);
      void this.admit(payload);
    });
    // Reconcile after listeners: an approval may arrive during the initial snapshot.
    for (const run of await commands.listShellRuns()) {
      this.upsert(run);
      if (run.phase === 'awaiting_pane') void this.admit(run);
    }
    return () => {
      changed();
      pane();
    };
  }
  forSession(session: string | null): ShellRun | undefined {
    return session
      ? [...this.runs].reverse().find((run) => run.session === session)
      : undefined;
  }
  get pending(): ShellRun[] {
    return this.runs.filter((run) => run.phase === 'awaiting_approval');
  }
}
export const shellRuns = new ShellRuns();
