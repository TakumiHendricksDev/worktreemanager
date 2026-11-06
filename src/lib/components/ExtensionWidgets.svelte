<script lang="ts">
  import { setContext } from 'svelte';
  import type { ExtensionWidget, ExtensionContext } from '../extensions';
  import { extensions, EXTENSION_PROTECTED } from '../state/extensions.svelte';
  import { errorMessage } from '../ipc/types';
  import Button from './ui/Button.svelte';
  import Banner from './ui/Banner.svelte';
  import Markdown from './Markdown.svelte';
  import ExtensionSetting from './ExtensionSetting.svelte';

  const {
    plugin,
    widgets,
    context,
  }: { plugin: string; widgets: ExtensionWidget[]; context: ExtensionContext } = $props();
  // Rendering a plugin's Markdown must not recursively insert that plugin into its own output.
  setContext(EXTENSION_PROTECTED, true);
  let problem = $state('');
  let busy = $state(false);
  async function act(id: string) {
    const action = extensions
      .contributions('actions', context)
      .find((c) => c.plugin === plugin && c.value.id === id);
    if (!action) return;
    busy = true;
    problem = '';
    try {
      await extensions.action(action, context);
    } catch (e) {
      problem = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

{#snippet body(list: ExtensionWidget[])}
  {#each list as widget, index (index)}
    {#if widget.kind === 'text'}<p>{extensions.text(widget.text, plugin, context)}</p>
    {:else if widget.kind === 'markdown'}<Markdown
        source={extensions.text(widget.text, plugin, context)}
      />
    {:else if widget.kind === 'action'}
      {@const action = extensions
        .contributions('actions', context)
        .find((c) => c.plugin === plugin && c.value.id === widget.action)}
      {#if action}<Button size="sm" disabled={busy} onclick={() => act(widget.action)}
          >{action.value.title}</Button
        >{/if}
    {:else if widget.kind === 'setting'}
      {@const setting = extensions
        .contributions('settings', context)
        .find((c) => c.plugin === plugin && c.value.id === widget.setting)}
      {#if setting}<ExtensionSetting {setting} {context} />{/if}
    {:else if widget.kind === 'group'}
      <section class="c-extension__group">
        <h4>{widget.title}</h4>
        {@render body(widget.children)}
      </section>
    {/if}
  {/each}
{/snippet}

{@render body(widgets)}
{#if problem}<Banner variant="error">{problem}</Banner>{/if}
