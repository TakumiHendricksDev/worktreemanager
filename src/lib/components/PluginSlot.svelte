<script lang="ts">
  /**
   * A workbench region a plugin view can replace: the built-in when nothing is chosen, the chosen
   * package's isolated view when one is, and nothing when the region is hidden.
   *
   * Nothing renders until the registry has loaded, so a replaced region never flashes its
   * built-in first. `visible` and `modal` pass through to the plugin view, which is a native
   * webview positioned over this box rather than a child of it.
   *
   * # Why this does not protect what it renders
   *
   * It did, and Svelte context reaches everything rendered through `children`, so wrapping the
   * whole workbench in one marked every feature in it protected and no contribution rendered
   * anywhere in the main window. A built-in region is ordinary host UI. Only `PluginView`, where a
   * package's own view is mounted, is protected.
   */
  import type { Snippet } from 'svelte';
  import type { PluginSlot } from '../plugins';
  import { plugins } from '../state/plugins.svelte';
  import PluginView from './PluginView.svelte';

  const {
    region,
    children,
    visible = true,
    modal = false,
  }: {
    region: PluginSlot;
    children: Snippet;
    visible?: boolean;
    modal?: boolean;
  } = $props();
  const binding = $derived(plugins.binding(region));
</script>

{#if plugins.ready && binding.kind === 'builtin'}
  {@render children()}
{:else if plugins.ready && binding.kind === 'plugin'}
  <PluginView slot={region} plugin={binding.plugin} view={binding.view} {visible} {modal} />
{/if}
