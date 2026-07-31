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
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, type Preflight, type Worktree } from '../ipc/types';
  import { workspace } from '../state/workspace.svelte';
  import Terminal from './Terminal.svelte';
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
  let busy = $state(false);
  let error = $state<string | null>(null);
  let done = $state<string | null>(null);
  let session = $state<string | null>(null);
  const acknowledged = $state<string[]>([]);

  const errors = $derived(preflight.filter((p) => p.severity === 'error'));
  const warns = $derived(preflight.filter((p) => p.severity === 'warn'));

  const canRemove = $derived(
    !busy && errors.every((p) => p.overridable && (force || acknowledged.includes(p.id))),
  );

  // Re-check when the branch checkbox changes: the unmerged-work warning only applies then.
  $effect(() => {
    const wantsBranch = deleteBranch;
    loading = true;
    void commands
      .removePreflight(projectId, worktree.id, wantsBranch)
      .then((items) => {
        preflight = items;
        error = null;
      })
      .catch((e) => {
        error = errorMessage(e);
      })
      .finally(() => {
        loading = false;
      });
  });

  function toggleAck(id: string, on: boolean) {
    const index = acknowledged.indexOf(id);
    if (on && index === -1) acknowledged.push(id);
    if (!on && index !== -1) acknowledged.splice(index, 1);
  }

  async function remove() {
    busy = true;
    error = null;
    try {
      const outcome = await commands.removeWorktree({
        projectId,
        worktreeId: worktree.id,
        deleteBranch,
        force,
        acknowledged: [...acknowledged],
      });

      if (outcome.kind === 'teardown_failed') {
        session = outcome.session;
        error =
          'A teardown step failed, so nothing was removed. ' +
          outcome.warnings.map((w) => w.message).join(' ');
        return;
      }

      done =
        `Removed ${worktree.dirname}` +
        (outcome.branchDeleted ? ` and deleted ${worktree.branch}.` : '.');
      if (outcome.warnings.length > 0) {
        done += ' ' + outcome.warnings.map((w) => w.message).join(' ');
      }
      await workspace.refreshWorktrees();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape' && !busy) {
      event.stopPropagation();
      onclose();
    }
  }
</script>

<Dialog title={done ? 'Removed' : 'Remove worktree'} {onclose} closeDisabled={busy} wide>
  {#snippet body()}
    {#if done}
      <p class="c-status--ok">{done}</p>
    {:else}
      <p class="c-remove__target"><code>{worktree.path}</code></p>
      {#if worktree.branch}
        <p class="c-remove__sub">on <code>{worktree.branch}</code></p>
      {:else}
        <p class="c-remove__sub c-status--muted">detached HEAD</p>
      {/if}

      <Choice
        checked={deleteBranch}
        disabled={busy || !worktree.branch}
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
          disabled={force || busy}
          onacknowledge={toggleAck}
        />
      {/if}

      <Choice size="sm" checked={force} disabled={busy} onchange={(on) => (force = on)}>
        Force — discard uncommitted and untracked files
      </Choice>

      <p class="c-note">
        The project's teardown steps run first, so containers are stopped and root-owned
        files are handed back before git touches the directory.
      </p>

      {#if error}
        <p class="c-status--danger">{error}</p>
      {/if}
    {/if}

    {#if session}
      <div class="c-dialog__terminal"><Terminal {session} /></div>
    {/if}
  {/snippet}

  {#snippet footer()}
    {#if done}
      <Button variant="accent" onclick={onclose}>Close</Button>
    {:else}
      <Button variant="neutral" onclick={onclose} disabled={busy}>Cancel</Button>
      <Button variant="danger-solid" onclick={remove} disabled={!canRemove}>
        {busy ? 'Removing…' : 'Remove'}
      </Button>
    {/if}
  {/snippet}
</Dialog>
