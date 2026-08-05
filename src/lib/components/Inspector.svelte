<script lang="ts">
  /**
   * Everything about the selected worktree that is not a session.
   *
   * # Why a dialog and not a rail
   *
   * A persistent rail would be a third region competing for `min-height: 0`, would need a `z-index`
   * for sticky section headers, and — decisively — would keep a revealed environment value on screen
   * across a worktree switch. `Dialog` **unmounts on close**, which re-masks for free: the property
   * `Detail`'s `$effect` on `worktree.id` used to protect is now a consequence of the shape.
   *
   * # The markup moved house unchanged
   *
   * `o-facts`, `.c-table`, `.c-table--env`, `.c-detail__masked`, `.c-detail__revealed`,
   * `.c-row-action`, `.c-detail__inherited` and the reveal/hide/copy handlers are the ones that were
   * in the Overview and Environment tabs, moved rather than rewritten. That is what keeps
   * `tests/env_masking.rs` proving exactly the property it proved before: no value reaches the
   * window except through `reveal_env_value`, one key at a time, read fresh from disk.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, type Worktree } from '../ipc/types';
  import Dialog from './ui/Dialog.svelte';

  const {
    worktree,
    projectId,
    onclose,
  }: {
    worktree: Worktree;
    projectId: string;
    onclose: () => void;
  } = $props();

  let copied = $state(false);
  let copiedKey = $state<string | null>(null);
  /** Values the user has explicitly revealed. Gone when this dialog closes, which is the point. */
  let revealed = $state<Record<string, string>>({});
  let revealError = $state<string | null>(null);

  async function copyPath() {
    try {
      await navigator.clipboard.writeText(worktree.path);
      copied = true;
      setTimeout(() => (copied = false), 1400);
    } catch {
      // Clipboard access can be denied; the path is selectable as a fallback.
    }
  }

  async function open(url: string) {
    try {
      await commands.openUrl(url);
    } catch {
      /* Nothing useful to do if the OS declines or the scheme is rejected. */
    }
  }

  async function reveal(key: string) {
    revealError = null;
    try {
      revealed[key] = await commands.revealEnvValue(projectId, worktree.id, key);
    } catch (e) {
      revealError = errorMessage(e);
    }
  }

  function hide(key: string) {
    delete revealed[key];
  }

  async function copyValue(key: string) {
    try {
      const value =
        revealed[key] ?? (await commands.revealEnvValue(projectId, worktree.id, key));
      await navigator.clipboard.writeText(value);
      copiedKey = key;
      setTimeout(() => (copiedKey = null), 1400);
    } catch (e) {
      revealError = errorMessage(e);
    }
  }
</script>

<Dialog title={worktree.title} {onclose} wide>
  {#snippet body()}
    <dl class="o-facts">
      <!-- First, because "where is this on disk" is the question the app exists to answer quickly. -->
      <dt>Path</dt>
      <dd class="c-detail__path-row">
        <code>{worktree.path}</code>
        <button class="c-row-action" onclick={copyPath}>{copied ? 'copied' : 'copy'}</button
        >
      </dd>

      <dt>Branch</dt>
      <dd>
        {#if worktree.branch}
          <code>{worktree.branch}</code>
        {:else}
          <!-- Never substitute the directory name here: they legitimately disagree. -->
          <span class="c-status--muted">detached HEAD</span>
        {/if}
      </dd>

      {#if worktree.head}
        <dt>HEAD</dt>
        <dd><code>{worktree.head}</code></dd>
      {/if}

      <dt>Status</dt>
      <dd>
        {#if worktree.dirty || worktree.untracked > 0 || worktree.staged > 0}
          <span class="c-detail__statuses">
            {#if worktree.staged > 0}<span class="c-status--ok"
                >{worktree.staged} staged</span
              >{/if}
            {#if worktree.dirty}<span class="c-status--warn">modified</span>{/if}
            {#if worktree.untracked > 0}
              <span class="c-status--muted">{worktree.untracked} untracked</span>
            {/if}
          </span>
        {:else}
          <span class="c-status--ok">clean</span>
        {/if}
      </dd>

      {#if worktree.ahead > 0 || worktree.behind > 0}
        <dt>Divergence</dt>
        <dd>
          <span class="c-status--info"
            >{worktree.ahead} ahead, {worktree.behind} behind</span
          >
        </dd>
      {/if}

      {#if worktree.locked}
        <dt>Locked</dt>
        <dd><span class="c-status--warn">{worktree.locked || 'no reason given'}</span></dd>
      {/if}

      {#if worktree.prunable}
        <dt>Stale</dt>
        <dd><span class="c-status--danger">{worktree.prunable}</span></dd>
      {/if}
    </dl>

    {#if worktree.table.length > 0}
      <h2 class="c-section-heading">Ports</h2>
      <table class="c-table">
        <tbody>
          {#each worktree.table as row (row.label)}
            <tr>
              <th>{row.label}</th>
              <td>
                {#if row.url}
                  <button
                    class="c-button c-button--link c-button--sm"
                    onclick={() => open(row.url ?? '')}>{row.value}</button
                  >
                {:else}
                  <code>{row.value}</code>
                {/if}
                {#if row.inherited}
                  <!-- An absent variable means the base value is in effect. Invisible unless we
                       say so, and then confusing. -->
                  <span
                    class="c-detail__inherited"
                    title="Not set for this worktree — the project default applies"
                  >
                    default
                  </span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    {#if worktree.env.length > 0}
      <h2 class="c-section-heading">Environment</h2>
      {#if revealError}
        <p class="c-status--danger">{revealError}</p>
      {/if}
      <p class="c-detail__env-note">
        No values are sent to this window. Reveal one at a time; each is read from disk when
        you ask for it and never kept. Closing this dialog re-masks everything.
      </p>
      <table class="c-table c-table--env">
        <tbody>
          {#each worktree.env as key (key)}
            <tr>
              <th><code>{key}</code></th>
              <td>
                {#if revealed[key] !== undefined}
                  <code class="c-detail__revealed">{revealed[key]}</code>
                  <button class="c-row-action" onclick={() => hide(key)}>hide</button>
                {:else}
                  <code class="c-detail__masked" aria-label="hidden value">••••••••</code>
                  <button class="c-row-action" onclick={() => reveal(key)}>reveal</button>
                {/if}
                <button class="c-row-action" onclick={() => copyValue(key)}>
                  {copiedKey === key ? 'copied' : 'copy'}
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/snippet}

  {#snippet footer()}
    <button class="c-button c-button--neutral c-button--md" onclick={onclose}>Close</button>
  {/snippet}
</Dialog>
