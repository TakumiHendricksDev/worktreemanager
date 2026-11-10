<script lang="ts">
  /**
   * The host's own strip across the top while a plugin draws the workbench or the window's navigation.
   *
   * # Why it exists at all
   *
   * Settings is where a misbehaving package is turned off, and a plugin that replaces the whole
   * workbench, or the title bar that holds the Settings button, could otherwise leave nothing on
   * screen that reaches it. So this strip is not a region and cannot be replaced: it offers the two
   * things the window needs whatever is installed — adding a repository and Plugins & recovery.
   *
   * # Why it is a drag region
   *
   * The window has `titleBarStyle: "Overlay"`, so there is no native title bar to drag by. When the
   * built-in `TitleBar` is replaced or hidden, this strip is the only thing at the top of the window,
   * and without `data-tauri-drag-region` the window could not be moved. Tauri starts a drag only
   * when the event's own target carries the attribute, so the buttons are unaffected.
   *
   * It names what is replaced. It used to say "Custom workbench" when only the title bar was.
   */
  import { plugins } from '../state/plugins.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import Button from './ui/Button.svelte';

  const { onaddproject }: { onaddproject: () => void } = $props();

  const workbench = $derived(plugins.binding('workbench').kind);
  const titlebar = $derived(plugins.binding('titlebar').kind);
  /** What the strip says is in charge, or null when the built-in title bar is on screen. */
  const replaced = $derived.by((): string | null => {
    if (!plugins.ready) return null;
    if (workbench === 'plugin') return 'Custom workbench';
    if (workbench === 'hidden') return 'Workbench hidden';
    if (titlebar === 'plugin') return 'Custom window navigation';
    if (titlebar === 'hidden') return 'Window navigation hidden';
    return null;
  });
</script>

{#if replaced}
  <header class="c-titlebar" data-tauri-drag-region>
    <div class="c-titlebar__gutter" data-tauri-drag-region></div>
    <div class="c-titlebar__identity" data-tauri-drag-region>
      <span class="c-titlebar__name" data-tauri-drag-region>{replaced}</span>
    </div>
    <div class="c-titlebar__actions">
      <Button onclick={onaddproject}>Add repository</Button>
      <Button onclick={() => settingsView.show('plugins')}>Plugins &amp; recovery</Button>
    </div>
  </header>
{/if}
