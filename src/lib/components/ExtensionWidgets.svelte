<script lang="ts">
  /**
   * A content contribution's body: the host-rendered widgets a manifest may declare.
   *
   * Text, Markdown, one of the package's own actions as a button, one of its settings as a field,
   * and titled groups of those. There is no HTML widget on purpose: custom markup belongs in an
   * isolated panel or view, where it cannot reach the host's DOM. Text is interpolated with
   * `${context:…}` and `${setting:…}` through the registry, which only lets allow-listed context
   * keys through.
   */
  import type { ExtensionContext, ExtensionWidget } from '../extensions';
  import { errorMessage } from '../ipc/types';
  import { extensions, protectFromExtensions } from '../state/extensions.svelte';
  import ExtensionSetting from './ExtensionSetting.svelte';
  import Markdown from './Markdown.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';

  const {
    plugin,
    widgets,
    context,
  }: { plugin: string; widgets: ExtensionWidget[]; context: ExtensionContext } = $props();
  // A plugin's Markdown would otherwise carry its Markdown parts, and every package's styles for
  // them, inside another package's content.
  protectFromExtensions();
  let problem = $state('');
  let busy = $state(false);

  async function act(id: string) {
    const action = extensions.action(plugin, id, context);
    if (!action) return;
    busy = true;
    problem = '';
    try {
      await extensions.perform(action, context);
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
      {@const action = extensions.action(plugin, widget.action, context)}
      {#if action}<Button size="sm" disabled={busy} onclick={() => act(widget.action)}
          >{action.value.title}</Button
        >{/if}
    {:else if widget.kind === 'setting'}
      {@const setting = extensions.setting(plugin, widget.setting, context)}
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
