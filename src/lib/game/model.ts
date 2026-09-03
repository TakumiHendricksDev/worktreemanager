/** The typed seam between WTM state and the vendored renderer. */

import type { Worktree } from '../ipc/types';
import type { Pane } from '../state/sessions.svelte';
import type { GameRepositoryRecord } from '../state/game-world.svelte';
import type { PaneStatus } from '../status';

export type ActorStatus =
  'blocked' | 'waiting' | 'working' | 'celebrating' | 'idle' | 'sleeping';

export interface GameActor {
  id: string;
  paneId: string;
  projectId: string;
  worktreeId: string;
  provider: string;
  label: string;
  title: string;
  model: string | null;
  paneStatus: PaneStatus;
  gameStatus: ActorStatus;
}

export interface GameJob {
  id: string;
  projectId: string;
  worktree: Worktree;
  actors: GameActor[];
  shells: number;
}

export interface GameRepository {
  id: string;
  name: string;
  root: string;
  usable: boolean;
  problem: string | null;
  jobs: GameJob[];
}

export interface GameSnapshot {
  repositories: GameRepository[];
}

const TO_GAME_STATUS: Record<PaneStatus, ActorStatus> = {
  failed: 'blocked',
  attention: 'waiting',
  working: 'working',
  done: 'celebrating',
  starting: 'idle',
  idle: 'idle',
  detached: 'sleeping',
  ended: 'sleeping',
};

export function buildGameSnapshot(
  records: readonly GameRepositoryRecord[],
  panes: readonly Pane[],
  statusOf: (pane: Pane) => PaneStatus,
  labelOf: (pane: Pane) => string,
): GameSnapshot {
  const byWorktree = new Map<string, Pane[]>();
  for (const pane of panes) {
    // `/btw` is an overlay belonging to its parent, not an independent worker in the world.
    if (pane.sideOf !== null) continue;
    const held = byWorktree.get(pane.worktreeId);
    if (held) held.push(pane);
    else byWorktree.set(pane.worktreeId, [pane]);
  }

  return {
    repositories: records.map(({ project, worktrees }) => ({
      id: project.id,
      name: project.name,
      root: project.root,
      usable: project.usable,
      problem: project.problem,
      jobs: worktrees.map((worktree) => {
        const residents = byWorktree.get(worktree.id) ?? [];
        const agents = residents.filter(
          (pane): pane is Pane & { kind: { kind: 'agent'; provider: string } } =>
            pane.kind.kind === 'agent',
        );
        return {
          id: worktree.id,
          projectId: project.id,
          worktree,
          shells: residents.filter((pane) => pane.kind.kind === 'shell').length,
          actors: agents.map((pane) => {
            const paneStatus = statusOf(pane);
            const label = labelOf(pane);
            return {
              id: pane.id,
              paneId: pane.id,
              projectId: project.id,
              worktreeId: worktree.id,
              provider: pane.kind.provider,
              label,
              title: pane.agentTitle?.trim() || label,
              model: pane.model,
              paneStatus,
              gameStatus: TO_GAME_STATUS[paneStatus],
            };
          }),
        };
      }),
    })),
  };
}
