/**
 * SQL on its way to the Database console — from an agent's reply, to the view that runs it.
 *
 * A store rather than a callback threaded through the tree, because the two ends are far apart:
 * the reply is a `Markdown` block four components deep in a pane, the console is `DatabaseSurface`,
 * and between them `App` has to switch the main view — which it owns — and select the worktree.
 * Each consumer remembers the last request id it acted on instead of clearing the request, so
 * neither can take it away before the other has seen it.
 *
 * A pane popped out into its own window has no Database view, so there the request travels to the
 * main window as an intent and starts again here.
 */

import { inPaneWindow, toMain } from '../window-role';

export interface ConsoleRequest {
  /** Increments per request, so the same SQL asked for twice is two requests. */
  id: number;
  projectId: string;
  worktreeId: string;
  sql: string;
}

class DatabaseConsole {
  request = $state<ConsoleRequest | null>(null);
  private issued = 0;

  /**
   * Show `sql` in this worktree's console and run it read-only. Read-only because the text came
   * from a model, not from the user's keyboard: a write fails and stays in the editor, for the
   * console's own Run to execute as written.
   */
  open(projectId: string, worktreeId: string, sql: string): void {
    if (inPaneWindow) {
      toMain({ kind: 'openInDatabase', projectId, worktreeId, sql });
      return;
    }
    this.issued += 1;
    this.request = { id: this.issued, projectId, worktreeId, sql };
  }
}

export const databaseConsole = new DatabaseConsole();
