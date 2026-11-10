import { workspaceLabels } from './state/workspace-labels.svelte';

/** Display identities never replace the directory id used by actions. */
export interface WorktreeIdentity {
  title: string;
  dirname: string;
  branch: string | null;
  path: string;
  issueKey: string | null;
  isMain: boolean;
  head: string | null;
  /**
   * A row standing in for a worktree the listing does not have yet, so its `branch` and `head` are
   * unknown rather than absent. See `arrangeFleet`.
   */
  placeholder?: boolean;
}

export function worktreeLabel(worktree: WorktreeIdentity): {
  key: string | null;
  name: string;
  detail: string;
} {
  // A placeholder knows only its folder. "Detached" is a claim about git, and saying it of a
  // worktree whose branch has simply not been listed yet put it on a checkout an agent was working
  // in until the next refresh, and only there.
  if (worktree.placeholder) {
    return {
      key: null,
      name: workspaceLabels.values[worktree.path] ?? worktree.dirname,
      detail: [worktree.dirname, worktree.path].join('\n'),
    };
  }
  const human = worktree.title !== worktree.dirname && worktree.title !== worktree.branch;
  const key = worktree.issueKey;
  let name = human
    ? worktree.title
    : worktree.isMain
      ? 'Main checkout'
      : worktree.branch === null
        ? `Detached${worktree.head ? ` · ${worktree.head.slice(0, 7)}` : ''}`
        : worktree.branch || worktree.dirname;
  if (key && !human) {
    const at = name.toLowerCase().indexOf(key.toLowerCase());
    if (at >= 0) name = name.slice(at + key.length);
    name = name.replace(/^[-_/\s]+/, '').replace(/[-_]+/g, ' ');
  } else if (key && name.toLowerCase().startsWith(key.toLowerCase())) {
    name = name.slice(key.length).replace(/^[-:·\s]+/, '');
  }
  return {
    key,
    name: workspaceLabels.values[worktree.path] ?? name,
    detail: [worktree.title, worktree.branch ?? 'Detached HEAD', worktree.path].join('\n'),
  };
}
