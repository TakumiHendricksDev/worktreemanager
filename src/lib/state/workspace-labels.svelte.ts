import { commands } from '../ipc/commands';

class WorkspaceLabels {
  values = $state<Record<string, string>>({});
  async init() {
    const stored = await commands.getPref('ui.workspace_labels');
    if (!stored) return;
    try {
      const value: unknown = JSON.parse(stored);
      if (value && typeof value === 'object' && !Array.isArray(value))
        this.values = Object.fromEntries(
          Object.entries(value).filter(([, v]) => typeof v === 'string'),
        );
    } catch {
      /* An invalid preference leaves the Git-derived labels intact. */
    }
  }
  async rename(path: string, name: string) {
    const next = { ...this.values };
    if (name.trim()) next[path] = name.trim();
    else delete next[path];
    await commands.setPref('ui.workspace_labels', JSON.stringify(next));
    this.values = next;
  }
}
export const workspaceLabels = new WorkspaceLabels();
