<script lang="ts">
  /**
   * Add a repository.
   *
   * This exists because the first version used `window.prompt()`, which a Tauri webview does
   * not implement — it returns `null` immediately, so the button appeared to do nothing at
   * all. `window.alert` and `window.confirm` are the same story. Nothing in this app may rely
   * on them.
   *
   * The path is typed rather than picked from a native dialog: the file-dialog plugin is
   * another dependency and another capability to grant, and a developer already knows the path
   * they want. Registration resolves whatever is typed to the repository root, so pasting a
   * subdirectory works.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import { workspace } from '../state/workspace.svelte';

  const { onclose }: { onclose: () => void } = $props();

  let path = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let input = $state<HTMLInputElement | null>(null);

  $effect(() => {
    input?.focus();
  });

  async function submit(event?: Event) {
    event?.preventDefault();
    const trimmed = path.trim().replace(/\/+$/, '');
    if (!trimmed) return;

    busy = true;
    error = null;
    try {
      // `~` is expanded in Rust, which actually knows HOME.
      await workspace.addProject(trimmed);
      onclose();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      event.stopPropagation();
      onclose();
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="scrim" onclick={onclose}></div>

<div class="dialog" role="dialog" aria-modal="true" aria-label="Add a repository">
  <form onsubmit={submit}>
    <header>
      <h2>Add a repository</h2>
      <button type="button" class="close" onclick={onclose} aria-label="Close">✕</button>
    </header>

    <div class="body">
      <label for="repo-path">Path to a git repository</label>
      <input
        id="repo-path"
        bind:this={input}
        bind:value={path}
        type="text"
        placeholder="~/Sites/your-repo"
        autocapitalize="off"
        autocorrect="off"
        spellcheck="false"
      />
      <p class="help">
        Any path inside the repository works — wtm resolves it to the root. Nothing is
        written to the repository itself.
      </p>
      {#if error}
        <p class="error">{error}</p>
      {/if}
    </div>

    <footer>
      <button type="button" class="secondary" onclick={onclose}>Cancel</button>
      <button type="submit" class="primary" disabled={busy || path.trim() === ''}>
        {busy ? 'Adding…' : 'Add'}
      </button>
    </footer>
  </form>
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
    width: min(520px, calc(100vw - 4rem));
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

  .close:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .body {
    padding: var(--sp-2) var(--sp-5) var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }

  label {
    font-size: var(--step--1);
    font-weight: 500;
  }

  input {
    width: 100%;
    padding: 7px 9px;
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    background: var(--bg-input);
    font-family: var(--font-mono);
    font-size: var(--step--1);
  }

  input:focus {
    border-color: var(--border-focus);
  }

  .help {
    color: var(--fg-muted);
    font-size: var(--step--2);
    line-height: 1.55;
  }

  .error {
    color: var(--danger);
    font-size: var(--step--1);
  }

  footer {
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

  .secondary:hover {
    background: var(--bg-hover);
  }

  .primary {
    background: var(--accent);
    color: var(--fg-on-accent);
  }

  .primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .primary:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
</style>
