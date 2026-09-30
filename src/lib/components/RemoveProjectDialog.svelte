<script lang="ts">
  /**
   * Stop listing a repository.
   *
   * Asked rather than done on the click, even though nothing on disk changes and adding it back is
   * one dialog away, because two things do not come back with it: its sidebar layout lives in the
   * project's entry in `config.toml` and is deleted with the entry, and every pane open in its
   * worktrees is closed. The second is the one that costs something — a running agent's turn is
   * lost, not paused.
   *
   * The dialog says what is kept as plainly as what is lost. "Remove" beside a repository reads as
   * deleting it, and the answer to that fear is the first sentence of the body, not a footnote.
   */
  import { errorMessage, type Project } from '../ipc/types';
  import { sessions } from '../state/sessions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';

  const { project, onclose }: { project: Project; onclose: () => void } = $props();

  let busy = $state(false);
  let error = $state<string | null>(null);

  // Side panes are left out for the reason `remember` leaves them out: a `/btw` card is an overlay
  // on its parent, and counting it would name a pane the user never opened as one.
  const open = $derived(
    sessions.panes.filter((p) => p.projectId === project.id && p.sideOf === null).length,
  );

  async function remove() {
    // Read before the await: `removeProject` selects the next project before it returns, and a
    // caller passing the active project would hand this dialog that one by the time it resumed.
    const { id, root } = project;
    busy = true;
    error = null;
    try {
      // Unregistered first, so a write that fails leaves the sessions running as well as listed.
      await workspace.removeProject(root);
      sessions.closeProject(id);
      onclose();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title="Remove repository" {onclose} closeDisabled={busy}>
  {#snippet body()}
    <p class="c-remove__target"><code>{project.root}</code></p>

    <p>
      wtm stops listing <strong>{project.name}</strong>. Nothing on disk is touched: the
      repository, its worktrees and their branches stay exactly where they are, and you can
      add it back at any time.
    </p>

    {#if open > 0}
      <p>
        {open === 1 ? 'The 1 pane' : `The ${open} panes`} open in its worktrees will close, ending
        any shells and agent sessions running there.
      </p>
    {/if}

    <p class="c-note">
      Its sidebar groups and favorites are forgotten, along with any overrides written for
      it in wtm's <code>config.toml</code>.
    </p>

    {#if error}
      <p class="c-status--danger">{error}</p>
    {/if}
  {/snippet}

  {#snippet footer()}
    <Button variant="neutral" onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="danger-solid" onclick={remove} disabled={busy}>
      {busy ? 'Removing…' : 'Remove from wtm'}
    </Button>
  {/snippet}
</Dialog>
