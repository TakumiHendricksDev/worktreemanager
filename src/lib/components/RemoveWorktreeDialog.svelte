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

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="scrim" onclick={() => !busy && onclose()}></div>

<div class="dialog" role="dialog" aria-modal="true" aria-label="Remove worktree">
  <header>
    <h2>{done ? 'Removed' : 'Remove worktree'}</h2>
    <button class="close" onclick={onclose} disabled={busy} aria-label="Close">✕</button>
  </header>

  <div class="body">
    {#if done}
      <p class="ok">{done}</p>
    {:else}
      <p class="target">
        <code>{worktree.path}</code>
      </p>
      {#if worktree.branch}
        <p class="sub">on <code>{worktree.branch}</code></p>
      {:else}
        <p class="sub muted">detached HEAD</p>
      {/if}

      <label class="check">
        <input
          type="checkbox"
          bind:checked={deleteBranch}
          disabled={busy || !worktree.branch}
        />
        <span>
          Also delete the branch
          {#if worktree.branch}<code>{worktree.branch}</code>{/if}
        </span>
      </label>

      {#if loading}
        <p class="muted">Checking…</p>
      {:else}
        {#each errors as item (item.id)}
          <div class="item">
            <span class="danger">✗ {item.message}</span>
            {#if item.hint}<span class="hint">{item.hint}</span>{/if}
            {#if item.overridable}
              <label class="check small">
                <input
                  type="checkbox"
                  checked={force || acknowledged.includes(item.id)}
                  disabled={force || busy}
                  onchange={(e) =>
                    toggleAck(item.id, (e.currentTarget as HTMLInputElement).checked)}
                />
                <span>Remove anyway</span>
              </label>
            {/if}
          </div>
        {/each}
        {#each warns as item (item.id)}
          <div class="item">
            <span class="warn">! {item.message}</span>
            {#if item.hint}<span class="hint">{item.hint}</span>{/if}
          </div>
        {/each}
      {/if}

      <label class="check small">
        <input type="checkbox" bind:checked={force} disabled={busy} />
        <span>Force — discard uncommitted and untracked files</span>
      </label>

      <p class="note">
        The project's teardown steps run first, so containers are stopped and root-owned
        files are handed back before git touches the directory.
      </p>

      {#if error}
        <p class="error">{error}</p>
      {/if}
    {/if}

    {#if session}
      <div class="termwrap"><Terminal {session} /></div>
    {/if}
  </div>

  <footer>
    {#if done}
      <button class="primary" onclick={onclose}>Close</button>
    {:else}
      <button class="secondary" onclick={onclose} disabled={busy}>Cancel</button>
      <button class="danger-btn" onclick={remove} disabled={!canRemove}>
        {busy ? 'Removing…' : 'Remove'}
      </button>
    {/if}
  </footer>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--bg-scrim);
    z-index: 10;
  }

  .dialog {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(600px, calc(100vw - 4rem));
    max-height: min(80vh, 780px);
    display: flex;
    flex-direction: column;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--r-xl);
    box-shadow: var(--shadow-lg);
    z-index: 11;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: var(--sp-4) var(--sp-5) var(--sp-2);
    flex: 0 0 auto;
  }

  h2 {
    font-size: var(--step-1);
    font-weight: 600;
  }

  .close {
    width: 26px;
    height: 26px;
    border-radius: var(--r-md);
    color: var(--fg-muted);
  }

  .close:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .body {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    padding: var(--sp-2) var(--sp-5) var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  .target code {
    font-size: var(--step--1);
    overflow-wrap: anywhere;
  }

  .sub {
    font-size: var(--step--1);
    color: var(--fg-muted);
    margin-bottom: var(--sp-2);
  }

  .check {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-size: var(--step--1);
  }

  .check.small {
    font-size: var(--step--2);
    color: var(--fg-muted);
  }

  .item {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--step--1);
    padding: var(--sp-1) 0;
  }

  .hint {
    color: var(--fg-muted);
    font-size: var(--step--2);
    line-height: 1.5;
  }

  .note {
    font-size: var(--step--2);
    color: var(--fg-muted);
    line-height: 1.55;
    margin-top: var(--sp-2);
  }

  .termwrap {
    min-height: 200px;
    display: flex;
    flex-direction: column;
    margin-top: var(--sp-2);
  }

  footer {
    flex: 0 0 auto;
    border-top: 1px solid var(--border);
    padding: var(--sp-3) var(--sp-5) var(--sp-4);
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
  }

  footer button {
    padding: 7px 14px;
    border-radius: var(--r-md);
    font-size: var(--step--1);
    font-weight: 500;
  }

  .secondary {
    border: 1px solid var(--border-strong);
    color: var(--fg);
  }

  .secondary:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .primary {
    background: var(--accent);
    color: var(--fg-on-accent);
  }

  .danger-btn {
    background: var(--danger);
    color: var(--gray-0);
  }

  .danger-btn:hover:not(:disabled) {
    filter: brightness(1.1);
  }

  footer button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .muted {
    color: var(--fg-muted);
    font-size: var(--step--1);
  }
  .warn {
    color: var(--warn);
  }
  .danger {
    color: var(--danger);
  }
  .error {
    color: var(--danger);
    font-size: var(--step--1);
  }
  .ok {
    color: var(--ok);
    font-size: var(--step--1);
  }
</style>
