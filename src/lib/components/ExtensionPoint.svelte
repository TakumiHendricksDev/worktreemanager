<script lang="ts">
  import type { Snippet } from 'svelte';
  import type { ExtensionContext } from '../extensions';
  import { extensionContext, extensions } from '../state/extensions.svelte';
  import ExtensionWidgets from './ExtensionWidgets.svelte';
  import ExtensionSetting from './ExtensionSetting.svelte';
  import ExtensionActions from './ExtensionActions.svelte';

  const inherited = extensionContext();
  const {
    target,
    context,
    children,
  }: { target: string; context?: ExtensionContext; children?: Snippet } = $props();
  const current = $derived(context ?? inherited());
  const content = $derived(
    extensions.contributions('content', current).filter((c) => c.value.target === target),
  );
  const replacement = $derived(
    extensions.winner(
      target,
      content.filter((c) => c.value.position === 'replace'),
    ),
  );
  const settings = $derived(
    extensions.contributions('settings', current).filter((c) => c.value.section === target),
  );
  const settingGroups = $derived(
    [...new Set(settings.map((setting) => setting.plugin))].map((plugin) => ({
      plugin,
      name: settings.find((setting) => setting.plugin === plugin)?.packageName ?? plugin,
      settings: settings.filter((setting) => setting.plugin === plugin),
    })),
  );
</script>

{#each content.filter((c) => c.value.position === 'before') as contribution (contribution.key)}
  <section
    class="c-extension"
    aria-label={contribution.value.title}
    data-wtm-contribution={contribution.key}
  >
    <ExtensionWidgets
      plugin={contribution.plugin}
      widgets={contribution.value.body}
      context={current}
    />
  </section>
{/each}
{#if replacement}
  <section
    class="c-extension"
    aria-label={replacement.value.title}
    data-wtm-contribution={replacement.key}
  >
    <ExtensionWidgets
      plugin={replacement.plugin}
      widgets={replacement.value.body}
      context={current}
    />
  </section>
{:else if children}{@render children()}{/if}
{#each content.filter((c) => c.value.position === 'after') as contribution (contribution.key)}
  <section
    class="c-extension"
    aria-label={contribution.value.title}
    data-wtm-contribution={contribution.key}
  >
    <ExtensionWidgets
      plugin={contribution.plugin}
      widgets={contribution.value.body}
      context={current}
    />
  </section>
{/each}
{#each settingGroups as group (group.plugin)}
  <section class="c-extension c-extension__group" aria-label={group.name}>
    <h3 class="c-extension__label">{group.name}</h3>
    {#each group.settings as setting (setting.key)}
      <ExtensionSetting {setting} context={current} />
    {/each}
  </section>
{/each}
<ExtensionActions {target} context={current} />
