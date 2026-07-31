<script lang="ts">
  /**
   * Add a repository.
   *
   * This exists because the first version used `window.prompt()`, which a Tauri webview does
   * not implement — it returns `null` immediately, so the button appeared to do nothing at
   * all. `window.alert` and `window.confirm` are the same story. Nothing in this app may rely
   * on them.
   *
   * Both a text field and a Browse… button, deliberately.
   *
   * This used to be typed-only, on the grounds that the dialog plugin is another dependency and
   * another capability and a developer already knows their own paths. That was wrong about who
   * uses it: knowing the path and having it *on the clipboard* are different things, and typing
   * one out is the slowest way to do this. So the picker is here now, and `dialog:allow-open` is
   * the one capability this app grants beyond its own commands — it returns a single
   * user-chosen path and nothing else. The reasoning that survives is the other half: the field
   * stays, because pasting a path you already have beats hunting for it in a file browser.
   *
   * Picking does not submit. Registration resolves whatever it is given to the repository root,
   * so choosing a subdirectory registers somewhere other than what you clicked — showing the
   * path first is the difference between that being a feature and being a surprise.
   */
  import { open } from '@tauri-apps/plugin-dialog';

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

  async function browse() {
    error = null;
    try {
      const picked = await open({
        directory: true,
        multiple: false,
        title: 'Choose a git repository',
      });
      // `null` is a cancelled dialog, which is not a failure and must not say anything.
      if (typeof picked === 'string') path = picked;
    } catch (e) {
      // Surfaced rather than swallowed: the only realistic cause is a missing capability, and
      // a Browse button that silently does nothing is the bug this file's header opens with.
      error = errorMessage(e);
    } finally {
      input?.focus();
    }
  }

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
      <div class="pathrow">
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
        <button type="button" class="browse" onclick={browse} disabled={busy}
          >Browse…</button
        >
      </div>
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

  /* The field takes the space; the button stays exactly as wide as its label. */
  .pathrow {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--sp-2);
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

  .browse {
    padding: 7px 12px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-md);
    color: var(--fg);
    font-size: var(--step--1);
    font-weight: 500;
    white-space: nowrap;
  }

  .browse:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .browse:disabled {
    opacity: 0.45;
    cursor: default;
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
