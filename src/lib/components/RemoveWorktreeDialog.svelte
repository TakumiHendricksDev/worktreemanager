<script lang="ts">
  /**
   * Remove a worktree.
   *
   * The shell script asks `Delete branch <x>? [y/n]` on stdin, with no `--yes` flag and a
   * `confirm()` helper that loops forever on EOF. wtm runs git itself, so that question becomes
   * the checkbox below — which is the entire reason `[remove] strategy` defaults to `native`.
   *
   * Preflight runs on open and again when the branch checkbox changes, because "this branch has
   * commits not in the base" only matters if you are deleting it. Nothing here mutates until
   * Remove is pressed.
   *
   * Remove closes the dialog rather than waiting in it: teardown can take minutes, and this is
   * the decision, not the work. `worktreeRemoval.run` carries the removal on and reports how it
   * ended — see the note there.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, type Preflight, type Worktree } from '../ipc/types';
  import { sessions } from '../state/sessions.svelte';
  import { worktreeRemoval } from '../state/worktree-removal.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';
  import PreflightList from './ui/PreflightList.svelte';

  const {
    projectId,
    worktree,
    onclose,
  }: { projectId: string; worktree: Worktree; onclose: () => void } = $props();

  let deleteBranch = $state(false);
  let force = $state(false);
  let preflight = $state<Preflight[]>([]);
  let loading = $state(true);
  let preflightReady = $state(false);
  let error = $state<string | null>(null);
  const acknowledged = $state<string[]>([]);

  const affected = $derived(
    sessions.panes.filter(
      (pane) => pane.projectId === projectId && pane.worktreeId === worktree.id,
    ),
  );

  const errors = $derived(preflight.filter((p) => p.severity === 'error'));
  const warns = $derived(preflight.filter((p) => p.severity === 'warn'));

  const canRemove = $derived(
    !loading &&
      preflightReady &&
      errors.every((p) => p.overridable && (force || acknowledged.includes(p.id))),
  );

  // Re-check when the branch checkbox changes: the unmerged-work warning only applies then.
  let previewSeq = 0;
  $effect(() => {
    const wantsBranch = deleteBranch;
    const seq = ++previewSeq;
    loading = true;
    preflightReady = false;
    void commands
      .removePreflight(projectId, worktree.id, wantsBranch)
      .then((items) => {
        if (seq !== previewSeq) return;
        preflight = items;
        preflightReady = true;
        error = null;
      })
      .catch((e) => {
        if (seq !== previewSeq) return;
        preflightReady = false;
        error = errorMessage(e);
      })
      .finally(() => {
        if (seq === previewSeq) loading = false;
      });
  });

  function toggleAck(id: string, on: boolean) {
    const index = acknowledged.indexOf(id);
    if (on && index === -1) acknowledged.push(id);
    if (!on && index !== -1) acknowledged.splice(index, 1);
  }

  function remove() {
    if (!canRemove) return;
    void worktreeRemoval.run(
      { projectId, worktree },
      { deleteBranch, force, acknowledged: [...acknowledged] },
    );
    onclose();
  }
</script>

<Dialog title="Remove this worktree?" {onclose} wide>
  {#snippet body()}
    <p>
      This ends the worktree’s agents, shells and browser panes before running teardown.
      {#if affected.length > 0}{affected.length} open {affected.length === 1
          ? 'pane will close'
          : 'panes will close'}.{/if}
    </p>
    <p class="c-remove__target"><code>{worktree.path}</code></p>
    {#if worktree.branch}
      <p class="c-remove__sub">on <code>{worktree.branch}</code></p>
    {:else}
      <p class="c-remove__sub c-status--muted">detached HEAD</p>
    {/if}

    <Choice
      checked={deleteBranch}
      disabled={!worktree.branch}
      onchange={(on) => (deleteBranch = on)}
    >
      Also delete the branch
      {#if worktree.branch}<code>{worktree.branch}</code>{/if}
    </Choice>

    {#if loading}
      <p class="c-status--muted">Checking…</p>
    {:else}
      <PreflightList
        items={[...errors, ...warns]}
        acknowledged={force ? errors.map((e) => e.id) : acknowledged}
        overrideLabel="Remove anyway"
        disabled={force}
        onacknowledge={toggleAck}
      />
    {/if}

    <Choice size="sm" checked={force} onchange={(on) => (force = on)}>
      Force — discard uncommitted and untracked files
    </Choice>

    <p class="c-note">
      The project's teardown steps run first, so containers are stopped and root-owned files
      are handed back before git touches the directory.
    </p>

    {#if error}
      <p class="c-status--danger">{error}</p>
    {/if}
  {/snippet}

  {#snippet footer()}
    <Button variant="neutral" onclick={onclose}>Cancel</Button>
    <Button variant="danger-solid" onclick={remove} disabled={!canRemove}>Remove</Button>
  {/snippet}
</Dialog>
