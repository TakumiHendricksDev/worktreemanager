<script lang="ts">
  /**
   * The "Open in …" split button.
   *
   * Left half runs the preferred tool; right half opens a menu of every tool wtm knows
   * about, and picking one both runs it and becomes the new preference.
   *
   * # Why the menu is a native `<select>`
   *
   * The same reasoning as the project switcher in `TitleBar.svelte`, which this deliberately
   * mirrors: a real `<select>` gets keyboard navigation, type-ahead, click-outside and
   * Escape for free, and renders the way the platform's menus are expected to. A hand-rolled
   * popover would mean re-implementing all four, plus a focus trap, plus `role="menu"` and
   * `aria-expanded` — with `svelte-check` as the only gate, since this repository has no JS
   * test runner.
   *
   * It is styled to read as the right half of a button, and stretched invisibly over the
   * chevron so the whole area is the hit target.
   *
   * # The sentinel, which is not decoration
   *
   * A `<select>` fires **no** `change` event when you re-pick the option already selected.
   * Bound to the preference in the obvious way, choosing your current default from the menu
   * would therefore do nothing at all — the "I clicked it and nothing happened" failure that
   * `AddProjectDialog.svelte` exists because of. So the value is always the empty sentinel,
   * and is reset to it after every pick.
   *
   * # Unavailable tools are shown, not hidden
   *
   * A picker that omits what you do not have never teaches anyone that wtm supports Zed, and
   * a disabled row reading *"no `code` on wtm's PATH"* is the fastest available diagnosis of
   * this app's most likely production failure — a bundled app that cannot see Homebrew.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage, type Opener } from '../ipc/types';
  import { workspace } from '../state/workspace.svelte';

  const {
    projectId,
    worktreeId,
  }: {
    projectId: string;
    worktreeId: string;
  } = $props();

  /** How long the "Opening…" confirmation stays up. */
  const OPENED_MS = 1600;

  let busy = $state(false);
  let opened = $state<string | null>(null);
  let failure = $state<string | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;

  const openers = $derived(workspace.openers);

  /**
   * The tool the primary half runs.
   *
   * Resolved from the id rather than held separately, so an uninstalled preference arrives
   * here with `available: false` and its own explanation — Rust deliberately returns the
   * stored choice rather than quietly substituting another one, because opening Zed under a
   * button labelled Cursor is worse than a failure you can act on.
   */
  const primary = $derived(
    openers.find((o) => o.id === workspace.preferredOpener) ??
      openers.find((o) => o.available) ??
      null,
  );

  /** Available first, then the rest, each group in catalogue order. */
  const grouped = $derived({
    available: openers.filter((o) => o.available),
    missing: openers.filter((o) => !o.available),
  });

  async function launch(opener: Opener) {
    if (busy) return;
    failure = null;

    if (!opener.available) {
      failure = `${opener.label} — ${opener.detail ?? 'not available'}`;
      return;
    }

    busy = true;
    try {
      await commands.openIn(projectId, worktreeId, opener.id);
      // Nothing in wtm changes on success, and the new window may open behind this one, so
      // say so briefly. Without it there is no way to tell "it worked" from "nothing
      // happened".
      opened = opener.label;
      clearTimeout(timer);
      timer = setTimeout(() => (opened = null), OPENED_MS);
    } catch (e) {
      failure = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  async function pick(event: Event) {
    const select = event.currentTarget as HTMLSelectElement;
    const chosen = openers.find((o) => o.id === select.value);
    // Reset before anything can await, so the control is ready for the next pick even if
    // the launch below fails or the user clicks again immediately.
    select.value = '';
    if (!chosen) return;

    // Written first, so a launch that fails leaves the button showing the tool whose error
    // is on screen — which is what makes retrying obvious.
    if (chosen.available) await workspace.setPreferredOpener(chosen.id);
    await launch(chosen);
  }

  /** Re-probe as the menu opens, so an editor installed since launch appears. */
  function onmenuopen() {
    void workspace.refreshOpeners();
  }

  $effect(() => {
    // Clearing on selection change: an error about the previous worktree is worse than no
    // error at all.
    void worktreeId;
    failure = null;
    opened = null;
  });
</script>

{#if primary}
  <div class="wrap">
    <div class="split" class:busy>
      <button
        class="primary"
        onclick={() => launch(primary)}
        disabled={busy}
        title={primary.available
          ? `Open this worktree in ${primary.label}`
          : (primary.detail ?? '')}
      >
        {#if opened}
          Opened in {opened}
        {:else}
          Open in {primary.label}{primary.available ? '' : ' ⚠'}
        {/if}
      </button>

      <div class="more">
        <span class="caret" aria-hidden="true">⌄</span>
        <label class="visually-hidden" for="open-in-picker">Open this worktree in…</label>
        <!--
          `value` is always the sentinel, never the preference — see the header. The
          sentinel option is selectable and simply does nothing, rather than `disabled`,
          because a disabled option cannot reliably be re-selected programmatically.
        -->
        <select
          id="open-in-picker"
          value=""
          onchange={pick}
          onmousedown={onmenuopen}
          onfocus={onmenuopen}
        >
          <option value="">Open in…</option>
          {#each grouped.available as opener (opener.id)}
            <option value={opener.id}>{opener.label}</option>
          {/each}
          {#each grouped.missing as opener (opener.id)}
            <!-- Listed but not selectable: the label is the point, the tooltip is the why. -->
            <option value={opener.id} disabled title={opener.detail ?? ''}>
              {opener.label} — not found
            </option>
          {/each}
        </select>
      </div>
    </div>

    {#if failure}
      <p class="failure" role="alert">{failure}</p>
    {/if}
  </div>
{/if}

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
  }

  /* One control, drawn as two: a shared border with a divider, so it reads as a split
     button rather than as two buttons that happen to be adjacent. */
  .split {
    display: flex;
    align-items: stretch;
    border: 1px solid var(--border);
    border-radius: var(--r-md);
    background: var(--bg-elevated);
    overflow: hidden;
  }

  .split.busy {
    opacity: 0.7;
  }

  .primary {
    padding: 4px 10px;
    color: var(--fg);
    font-size: var(--step--2);
    white-space: nowrap;
    transition: background var(--dur-fast) var(--ease);
  }

  .primary:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .more {
    position: relative;
    display: grid;
    place-items: center;
    width: 22px;
    border-left: 1px solid var(--border);
    color: var(--fg-muted);
    transition: background var(--dur-fast) var(--ease);
  }

  .more:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .caret {
    font-size: var(--step--2);
    line-height: 1;
    pointer-events: none;
  }

  /* The real control, stretched invisibly over the caret. A bare select would size itself
     to its widest option — here a "not found" row — which would strand the caret a couple
     of hundred pixels away from the button it belongs to. */
  .more select {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    opacity: 0;
    cursor: pointer;
    appearance: none;
  }

  .more:focus-within {
    background: var(--bg-hover);
    outline: 1px solid var(--border-focus);
    outline-offset: -1px;
  }

  .failure {
    max-width: 42ch;
    color: var(--danger);
    font-size: var(--step--2);
    line-height: 1.4;
    text-align: right;
  }
</style>
