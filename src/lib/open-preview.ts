/**
 * Open a service's preview in a browser pane, only once it has answered.
 *
 * Shared by the Environment tab and the workspace header's Open preview, so the two cannot open
 * different things. The address comes from Rust's `verify_service_preview`, which renders the
 * preview, refuses anything not on this machine, and fetches it; a URL that did not answer is never
 * handed to a pane. The pane is the worktree's, so the window then goes to the worktree, where
 * panes are drawn: the Work view draws none.
 */

import { environments } from './state/environments.svelte';
import { sessions } from './state/sessions.svelte';

export async function openPreview(
  projectId: string,
  worktreeId: string,
  service: string,
  arrive: (worktreeId: string) => void,
): Promise<void> {
  const verified = await environments.verify(projectId, worktreeId, service);
  if (!verified) return;
  await sessions.openBrowser(projectId, worktreeId, verified.url);
  arrive(worktreeId);
}
