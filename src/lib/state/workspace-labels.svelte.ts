/**
 * Display names for workspaces, set by `ui.workspace.rename`, keyed by worktree path.
 *
 * A label never changes a workspace's identity or directory: `worktreeLabel` shows it in place of
 * the Git-derived name, and every action still uses the path. Stored as one JSON preference.
 */
import { commands } from '../ipc/commands';

function parse(stored: string | null): Record<string, string> {
  if (!stored) return {};
  try {
    const value: unknown = JSON.parse(stored);
    if (value && typeof value === 'object' && !Array.isArray(value))
      return Object.fromEntries(
        Object.entries(value).filter(
          (entry): entry is [string, string] => typeof entry[1] === 'string',
        ),
      );
  } catch {
    /* An invalid preference leaves the Git-derived labels showing. */
  }
  return {};
}

class WorkspaceLabels {
  values = $state<Record<string, string>>({});
  #writes: Promise<void> = Promise.resolve();

  /**
   * Read the preference, including when it has been cleared — an early return there used to keep
   * the old labels. Never rejects: a config that cannot be read keeps what is showing, and boot
   * waits on this.
   */
  async init() {
    try {
      this.values = parse(await commands.getPref('ui.workspace_labels'));
    } catch {
      /* Labels are cosmetic; the Git-derived names still identify every workspace. */
    }
  }

  /**
   * One read-modify-write at a time, each starting from the last one's result. Two quick renames
   * of different workspaces used to both start from the same map, and the second write lost the
   * first label.
   */
  rename(path: string, name: string): Promise<void> {
    const write = this.#writes.then(async () => {
      const next = { ...this.values };
      if (name.trim()) next[path] = name.trim();
      else delete next[path];
      await commands.setPref('ui.workspace_labels', JSON.stringify(next));
      this.values = next;
    });
    this.#writes = write.catch(() => {});
    return write;
  }
}
export const workspaceLabels = new WorkspaceLabels();
