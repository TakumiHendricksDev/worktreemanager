<script lang="ts">
  import { setContext as setExtensionContext } from 'svelte';
  import { EXTENSION_PROTECTED } from '../state/extensions.svelte';
  setExtensionContext(EXTENSION_PROTECTED, true);
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import { plugins } from '../state/plugins.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { browsers } from '../state/browsers.svelte';
  import { overlay } from '../state/overlay.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import { theme } from '../state/theme.svelte';
  import type { PluginSlot } from '../plugins';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  const {
    slot,
    plugin,
    view,
    visible,
    modal,
  }: { slot: PluginSlot; plugin: string; view: string; visible: boolean; modal: boolean } =
    $props();
  let box = $state<HTMLElement | null>(null);
  let id = $state<string | null>(null);
  let error = $state<string | null>(null);
  let retry = $state(0);
  const failure = $derived(error ?? browsers.viewOf(id)?.loadError ?? null);
  $effect(() => {
    void theme.applied;
    void browsers.viewOf(id)?.loading;
    if (!id) return;
    const styles = getComputedStyle(document.documentElement);
    const tokens = Object.fromEntries(
      ['accent', 'bg', 'fg', 'border', 'fg-muted', 'bg-elevated', 'danger', 'font-ui'].map(
        (name) => [name, styles.getPropertyValue(`--${name}`).trim()],
      ),
    );
    void commands.browserSetTheme(id, tokens).catch(() => {});
  });
  $effect(() => {
    const directory = plugins.state.registry.installed[plugin]?.current.directory;
    const project = workspace.activeProjectId ?? '';
    const worktree = workspace.selectedWorktreeId ?? '';
    const attempt = retry;
    let gone = false;
    let owned: string | null = null;
    if (directory && attempt >= 0) {
      error = null;
      id = null;
      void commands
        .pluginView(slot, plugin, view, project, worktree)
        .then((page) => {
          owned = page.id;
          if (gone) void commands.closeBrowser(page.id).catch(() => {});
          else id = page.id;
        })
        .catch((e) => {
          if (!gone) error = errorMessage(e);
        });
    }
    return () => {
      gone = true;
      if (owned) void commands.closeBrowser(owned).catch(() => {});
    };
  });
  $effect(() => {
    const browser = id;
    const node = box;
    if (!browser || !node) return;
    // A settings view lives inside one modal; only another modal should cover that view.
    const shown =
      visible && !failure && overlay.modals <= (modal ? 1 : 0) && !overlay.paneDrag;
    let frame = 0;
    let last = '';
    const place = () => {
      const r = node.getBoundingClientRect();
      const bounds =
        shown && r.width > 0 && r.height > 0
          ? { x: r.x, y: r.y, w: r.width, h: r.height }
          : null;
      const key = JSON.stringify(bounds);
      if (last !== key) {
        last = key;
        void commands.browserSetBounds(browser, bounds).catch(() => {});
      }
    };
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(place);
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(node);
    observer.observe(document.documentElement);
    window.addEventListener('resize', schedule);
    document.addEventListener('scroll', schedule, true);
    schedule();
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      window.removeEventListener('resize', schedule);
      document.removeEventListener('scroll', schedule, true);
      void commands.browserSetBounds(browser, null).catch(() => {});
    };
  });
</script>

<section
  class="c-plugin-view"
  class:is-toolbar={slot === 'titlebar' || slot === 'worktree_bar'}
  hidden={!visible}
  bind:this={box}
  aria-label="Plugin view"
>
  {#if failure}
    <Banner>{failure}</Banner>
    <div class="o-row">
      <Button onclick={() => retry++}>Retry</Button><Button
        onclick={() => settingsView.show('plugins')}>Plugin settings</Button
      >
    </div>
  {:else if !id}<p class="c-plugins__muted">Loading plugin view…</p>{/if}
</section>
