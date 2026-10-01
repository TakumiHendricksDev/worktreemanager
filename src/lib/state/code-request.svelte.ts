/**
 * A file on its way to the Code tab — from a link in an agent's reply, to the view that opens it.
 *
 * The same shape as `database-console.svelte.ts`, for the same reason: the link is four components
 * deep in a pane, the tab is `CodeSurface`, and between them `App` has to switch the main view and
 * select the worktree. Each consumer remembers the last request id it acted on rather than clearing
 * the request, so neither can take it away before the other has seen it.
 *
 * A pane in a window of its own has no Code tab, so there the request travels to the main window as
 * an intent and starts again here.
 */

import { inPaneWindow, toMain } from '../window-role';

export interface CodeRequest {
  /** Increments per request, so opening the same line twice is two requests. */
  id: number;
  projectId: string;
  worktreeId: string;
  path: string;
  line: number | null;
}

class CodeRequests {
  request = $state<CodeRequest | null>(null);
  private issued = 0;

  open(projectId: string, worktreeId: string, path: string, line: number | null): void {
    if (inPaneWindow) {
      toMain({ kind: 'openInCode', projectId, worktreeId, path, line });
      return;
    }
    this.issued += 1;
    this.request = { id: this.issued, projectId, worktreeId, path, line };
  }
}

export const codeRequests = new CodeRequests();
