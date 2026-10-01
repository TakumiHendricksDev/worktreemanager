<script lang="ts">
  /**
   * The root of a window holding one popped-out pane.
   *
   * # What this starts, and what it deliberately does not
   *
   * The same stores as `App`, minus every one whose job is the whole app rather than one pane.
   * `attention` is the main window's — it tells the user about every pane, this one included, and
   * two windows judging the same event would post it twice. `updates` would ask about a release
   * once per window. `workspace.init` would list projects, land on the last one and write it back
   * as the last one. What this window needs from `workspace` — the pane's own worktree, for a
   * browser's links — the main window hands over, and `workspace.seed` takes it.
   *
   * # Why the title bar is not `TitleBar`
   *
   * `TitleBar` is the project switcher. A pane window belongs to one worktree and cannot switch
   * it, so its bar is only what the OS window needs — somewhere to drag, and room for the traffic
   * lights — plus where the pane is and the way back. It wears the same `c-titlebar` classes, so the
   * two bars are the same height and the traffic lights sit where `tauri.conf.json` puts them.
   */
  import { onMount } from 'svelte';

  import SessionPane from './lib/components/SessionPane.svelte';
  import Banner from './lib/components/ui/Banner.svelte';
  import Button from './lib/components/ui/Button.svelte';
  import Icon from './lib/components/ui/Icon.svelte';
  import { commands } from './lib/ipc/commands';
  import { browsers } from './lib/state/browsers.svelte';
  import { composerPrefs } from './lib/state/composer.svelte';
  import { dictation } from './lib/state/dictate.svelte';
  import { initPaneWindow, putBackFromWindow } from './lib/state/pane-windows.svelte';
  import { sessions } from './lib/state/sessions.svelte';
  import { theme } from './lib/state/theme.svelte';
  import { usage } from './lib/state/usage.svelte';
  import { workspace } from './lib/state/workspace.svelte';
  import { ownPaneId } from './lib/window-role';

  const pane = $derived(sessions.paneById(ownPaneId));
  const worktree = $derived(
    workspace.worktrees.find((candidate) => candidate.id === pane?.worktreeId) ?? null,
  );
  /** Set when there is no pane to show, so the window says why rather than staying blank. */
  let lost = $state(false);

  onMount(() => {
    let gone = false;
    let offSessions: (() => void) | null = null;
    let offWindow: (() => void) | null = null;
    // The pane's context card shows its agent's limits, and this window has its own copy of the
    // store. Seeded from Rust's record, so it starts with what the main window already knows.
    const offUsage = usage.init();

    void (async () => {
      await theme.init();
      await composerPrefs.init();
      await browsers.init();
      await dictation.init();
      if (gone || ownPaneId === null) return;

      // Subscribed before the pane is adopted, for the reason `adopt` states: the replay is asked
      // for after the listeners exist, so nothing the session says in between falls through.
      offSessions = await sessions.init();
      const view = await commands.paneWindowState().catch(() => null);
      if (gone) return;
      if (!view || !(await sessions.adoptWindowState(view.state))) {
        lost = true;
        return;
      }
      offWindow = await initPaneWindow(ownPaneId);
    })();

    return () => {
      gone = true;
      offWindow?.();
      offSessions?.();
      void offUsage.then((off) => off());
    };
  });
</script>

<div class="c-shell c-pane-window">
  <header class="c-titlebar" data-tauri-drag-region>
    <div class="c-titlebar__gutter" data-tauri-drag-region></div>
    <div class="c-titlebar__identity" data-tauri-drag-region>
      {#if pane}
        <span class="c-pane-window__title" data-tauri-drag-region>
          {sessions.labelOf(pane)}
        </span>
        <span class="c-titlebar__root" data-tauri-drag-region>
          {worktree?.title ?? pane.worktreeId}
        </span>
      {/if}
    </div>
    <div class="c-titlebar__actions">
      {#if pane}
        <!--
          Labelled, where the tile's control is an icon: this is the one action this bar exists for,
          and a window holding one pane has the room to say it. Closing the window does the same —
          see `pane_windows.rs` for why closing never ends the session.
        -->
        <Button
          variant="neutral"
          size="sm"
          title="Put this pane back in the main window. Closing this window does the same."
          onclick={() => ownPaneId && void putBackFromWindow(ownPaneId)}
        >
          <Icon name="pop-in" size={12} />
          Put back
        </Button>
      {/if}
    </div>
  </header>

  {#if sessions.error}
    <Banner>
      {sessions.error}
      {#snippet action()}
        <Button
          variant="inline"
          onclick={() => (sessions.error = null)}
          ariaLabel="Dismiss"
        >
          <Icon name="close" size={12} />
        </Button>
      {/snippet}
    </Banner>
  {/if}

  <main class="c-pane-window__body">
    {#if pane}
      <!-- Keyed as `SessionTree` keys a tile, so a Restart here remounts the same way it does there. -->
      {#key `${pane.id}:${pane.generation}`}
        <SessionPane {pane} visible={true} host={{ kind: 'window' }} />
      {/key}
    {:else if lost}
      <p class="c-pane-window__lost">
        This window has lost track of its pane. Close it; the session is still in the main
        window.
      </p>
    {/if}
  </main>
</div>
