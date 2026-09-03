/**
 * Every registered repository and worktree, for the optional world overview.
 *
 * `workspace` intentionally keeps only the active project's list. The world is the one screen that
 * needs all of them, so it owns a separate read-only projection and asks the existing command for
 * each usable project. There is no scanner and no timer: it refreshes when the world opens, when the
 * window regains focus, and after WTM itself mutates the catalogue.
 */

import { commands } from '../ipc/commands';
import type { Project, Worktree } from '../ipc/types';
import { workspace } from './workspace.svelte';

export interface GameRepositoryRecord {
  project: Project;
  worktrees: Worktree[];
  error: string | null;
}

const MAX_CONCURRENT_LISTS = 4;

async function mapLimited<T, R>(
  values: readonly T[],
  limit: number,
  visit: (value: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(values.length);
  let cursor = 0;

  async function worker(): Promise<void> {
    for (;;) {
      const index = cursor++;
      const value = values[index];
      if (value === undefined) return;
      results[index] = await visit(value);
    }
  }

  const count = Math.min(limit, values.length);
  await Promise.all(Array.from({ length: count }, () => worker()));
  return results;
}

class GameWorld {
  repositories = $state<GameRepositoryRecord[]>([]);
  loading = $state(false);
  error = $state<string | null>(null);

  /** A late refresh may not replace a newer one. */
  private epoch = 0;

  async refresh(): Promise<void> {
    const epoch = ++this.epoch;
    const projects = [...workspace.projects];
    const previous = new Map(
      this.repositories.map((record) => [record.project.id, record]),
    );
    this.loading = this.repositories.length === 0;

    try {
      const records = await mapLimited(projects, MAX_CONCURRENT_LISTS, async (project) => {
        if (!project.usable) {
          return { project, worktrees: [], error: project.problem };
        }
        try {
          const worktrees = await commands.listWorktrees(project.id);
          return { project, worktrees, error: null };
        } catch (cause) {
          const held = previous.get(project.id)?.worktrees ?? [];
          const message = cause instanceof Error ? cause.message : String(cause);
          return { project, worktrees: held, error: message };
        }
      });

      if (epoch !== this.epoch) return;
      this.repositories = records;
      const failures = records.filter((record) => record.error !== null).length;
      this.error = failures > 0 ? `${failures} repositories could not be refreshed.` : null;
    } catch (cause) {
      if (epoch !== this.epoch) return;
      this.error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (epoch === this.epoch) this.loading = false;
    }
  }
}

export const gameWorld = new GameWorld();
