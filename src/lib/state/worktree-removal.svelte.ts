/**
 * All removal entry points share one captured target and the existing confirmation dialog.
 *
 * # Why the removal outlives its dialog
 *
 * Teardown runs the project's own steps (`docker compose down -v` among them), and it can take
 * minutes. The dialog used to wait for all of it with Cancel and Escape disabled, and a modal
 * nobody can close freezes the whole window as far as the person using it can tell. Only the
 * decision needs a modal. Once Remove is pressed there is nothing left to decide, so the dialog
 * closes and the removal carries on here, where no component's lifetime can cut it short. The
 * worktree's row and its view read `isRemoving` meanwhile, and are locked until it ends.
 */
import { tick } from 'svelte';
import { commands } from '../ipc/commands';
import { errorMessage } from '../ipc/types';
import { revalidateWorktree, type WorktreeTarget } from '../worktree-menu';
import { attention } from './attention.svelte';
import { fleet } from './fleet.svelte';

/** What the dialog decided, beyond which worktree. */
export interface RemovalChoice {
  deleteBranch: boolean;
  force: boolean;
  acknowledged: string[];
}

class WorktreeRemoval {
  target = $state<WorktreeTarget | null>(null);
  /** Removals under way from the dialog, as `projectId` + NUL + `worktreeId`. */
  private running = $state<string[]>([]);
  private epoch = 0;
  private origin: HTMLElement | null = null;
  private neighbors: HTMLElement[] = [];

  /**
   * Whether this worktree is on its way out, whoever asked.
   *
   * Home's removals count as well. They run through the same `remove_now`, and a row that reads as
   * usable while its directory is being torn down would be wrong whichever button started it.
   */
  isRemoving(projectId: string, worktreeId: string): boolean {
    return (
      this.running.includes(key(projectId, worktreeId)) ||
      fleet.jobs.some(
        (job) =>
          job.kind === 'remove' &&
          job.phase === 'running' &&
          job.projectId === projectId &&
          job.worktree === worktreeId,
      )
    );
  }

  async request(target: WorktreeTarget): Promise<void> {
    if (this.target || this.isRemoving(target.projectId, target.worktree.id)) return;
    const epoch = ++this.epoch;
    const origin =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const rows = [
      ...(origin?.closest('[role="tree"]')?.querySelectorAll<HTMLElement>('[data-nav]') ??
        []),
    ];
    const index = rows.findIndex((row) => row === origin || row.contains(origin));
    try {
      const current = await revalidateWorktree(target);
      if (epoch !== this.epoch) return;
      this.origin = origin;
      this.neighbors = [...rows.slice(index + 1), ...rows.slice(0, index).reverse()];
      this.target = current;
    } catch (e) {
      attention.notice('Could not open removal', errorMessage(e));
    }
  }

  close(): void {
    this.epoch += 1;
    this.target = null;
    void tick().then(() => {
      // A row being removed is still on screen, but it no longer takes focus or arrow keys.
      const survivor = [this.origin, ...this.neighbors].find(
        (row) =>
          row?.isConnected &&
          row.getClientRects().length > 0 &&
          row.getAttribute('aria-disabled') !== 'true',
      );
      survivor?.focus();
      this.origin = null;
      this.neighbors = [];
    });
  }

  /**
   * Remove a worktree the dialog has confirmed, without anyone waiting on it.
   *
   * Says something only when there is something to say: a clean removal is announced by the row
   * going, and a toast nobody needs would still have to be dismissed by hand. Warnings and
   * failures get a notice, because by the time they arrive the dialog that would have shown them
   * is long gone.
   */
  async run(target: WorktreeTarget, choice: RemovalChoice): Promise<void> {
    const { projectId, worktree } = target;
    const id = key(projectId, worktree.id);
    if (this.running.includes(id)) return;
    this.running = [...this.running, id];
    const name = worktree.dirname;
    let outcome;
    try {
      outcome = await commands.removeWorktree({
        projectId,
        worktreeId: worktree.id,
        ...choice,
      });
    } catch (e) {
      this.settle(id);
      attention.notice(`Could not remove ${name}`, errorMessage(e));
      return;
    }
    const warnings = outcome.warnings.map((w) => w.message).join(' ');
    if (outcome.kind === 'teardown_failed') {
      this.settle(id);
      attention.notice(
        `Could not remove ${name}`,
        `A teardown step failed, so nothing was removed. ${warnings}`.trim(),
      );
      return;
    }
    // Still marked while the lists catch up, so the row cannot flicker back to usable just before
    // it goes.
    try {
      await fleet.removed(projectId, worktree.id);
    } finally {
      this.settle(id);
    }
    if (warnings) attention.notice(`Removed ${name}`, warnings);
  }

  private settle(id: string): void {
    this.running = this.running.filter((r) => r !== id);
  }
}

function key(projectId: string, worktreeId: string): string {
  return `${projectId}\u0000${worktreeId}`;
}

export const worktreeRemoval = new WorktreeRemoval();
