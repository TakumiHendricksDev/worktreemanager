/** Display identities never replace the directory id used by actions. */
export interface WorktreeIdentity {
  title: string;
  dirname: string;
  branch: string | null;
  path: string;
  issueKey: string | null;
  isMain: boolean;
  head: string | null;
}

export function worktreeLabel(worktree: WorktreeIdentity): {
  key: string | null;
  name: string;
  detail: string;
} {
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
    name,
    detail: [worktree.title, worktree.branch ?? 'Detached HEAD', worktree.path].join('\n'),
  };
}
