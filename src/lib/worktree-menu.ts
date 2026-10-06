/** Actions capture a directory identity; changing the selected row never retargets them. */
import { commands } from './ipc/commands';
import { errorMessage, type Link, type Worktree } from './ipc/types';
import { item, separator, type MenuEntry } from './native-menu';
import { attention } from './state/attention.svelte';
import { workspace } from './state/workspace.svelte';

export interface WorktreeTarget {
  projectId: string;
  worktree: Worktree;
}
export async function resolveWorktreeTarget(
  projectId: string,
  worktreeId: string,
): Promise<WorktreeTarget> {
  const worktree = (await commands.listWorktrees(projectId)).find(
    (w) => w.id === worktreeId,
  );
  if (!worktree)
    throw new Error('This worktree is no longer listed. Refresh and try again.');
  return { projectId, worktree };
}
export async function revalidateWorktree(target: WorktreeTarget): Promise<WorktreeTarget> {
  const current = await resolveWorktreeTarget(target.projectId, target.worktree.id);
  if (
    current.worktree.path !== target.worktree.path ||
    current.worktree.branch !== target.worktree.branch ||
    current.worktree.isMain !== target.worktree.isMain
  ) {
    throw new Error('This worktree changed while the menu was open. Open its menu again.');
  }
  return current;
}
export function worktreeAction(action: () => Promise<unknown>): void {
  void action().catch((e) => attention.notice('Worktree action failed', errorMessage(e)));
}

export async function openWorktreeLink(target: WorktreeTarget, link: Link): Promise<void> {
  const current = await revalidateWorktree(target);
  if (
    !current.worktree.links.some(
      (candidate) => candidate.label === link.label && candidate.url === link.url,
    )
  ) {
    throw new Error('This configured link changed. Open the worktree menu again.');
  }
  await commands.openUrl(link.url);
  workspace.rememberLink(target.projectId, link.label);
}

export function worktreeActions(target: WorktreeTarget): MenuEntry[] {
  const { worktree } = target;
  return [
    worktree.links.length > 0
      ? {
          kind: 'submenu',
          text: 'Open links',
          items: worktree.links.map((link) =>
            item(`${link.label} — ${link.url}`, () =>
              worktreeAction(() => openWorktreeLink(target, link)),
            ),
          ),
        }
      : item('No configured links', () => {}, false),
    separator,
    item(
      worktree.branch === null ? 'Copy branch name — detached HEAD' : 'Copy branch name',
      () =>
        worktreeAction(async () => {
          const current = await revalidateWorktree(target);
          if (current.worktree.branch !== null)
            await navigator.clipboard.writeText(current.worktree.branch);
        }),
      worktree.branch !== null,
    ),
    item('Copy directory path', () =>
      worktreeAction(async () => {
        const current = await revalidateWorktree(target);
        await navigator.clipboard.writeText(current.worktree.path);
      }),
    ),
  ];
}
