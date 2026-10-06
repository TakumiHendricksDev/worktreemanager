/** All removal entry points share one captured target and the existing confirmation dialog. */
import { tick } from 'svelte';
import { errorMessage } from '../ipc/types';
import { revalidateWorktree, type WorktreeTarget } from '../worktree-menu';
import { attention } from './attention.svelte';

class WorktreeRemoval {
  target = $state<WorktreeTarget | null>(null);
  private epoch = 0;
  private origin: HTMLElement | null = null;
  private neighbors: HTMLElement[] = [];

  async request(target: WorktreeTarget): Promise<void> {
    if (this.target) return;
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
      const survivor = [this.origin, ...this.neighbors].find(
        (row) => row?.isConnected && row.getClientRects().length > 0,
      );
      survivor?.focus();
      this.origin = null;
      this.neighbors = [];
    });
  }
}
export const worktreeRemoval = new WorktreeRemoval();
