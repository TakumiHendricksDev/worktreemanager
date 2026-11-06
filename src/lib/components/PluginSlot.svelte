<script lang="ts">
  import { setContext as setExtensionContext } from 'svelte';
  import { EXTENSION_PROTECTED } from '../state/extensions.svelte';
  setExtensionContext(EXTENSION_PROTECTED, true);
  import type { Snippet } from 'svelte';
  import { plugins } from '../state/plugins.svelte';
  import type { PluginSlot } from '../plugins';
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
