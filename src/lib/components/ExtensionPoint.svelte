<script lang="ts">
  /**
   * Where plugin content joins a built-in feature: one side of one semantic surface.
   *
   * A feature places `before` at the top of its own container and `after` at the bottom. `after`
   * also carries the surface's plugin settings and its Plugin actions button, when the catalog
   * says the surface takes them, so a surface has exactly one of each.
   *
   * # Why it does not wrap the feature
   *
   * It used to, and rendered the content as siblings of whatever it wrapped. Wrapped round a list
   * item that put a `<section>` inside an `<ol>`; round a Dialog it put one outside the portal, on
   * the page behind; round a surface hidden by class it put one on every other view. The feature
   * knows which container its content belongs in, so the feature places it. Nothing it wraps can be
   * replaced, either: no surface in the catalog accepts `replace`, so content only adds.
   *
   * # Context
   *
   * Inherited from the nearest `provideExtensionContext`, the pane a component is in. Pass
   * `context` only to add identity the feature knows and its pane does not, such as a message.
   * `null` there, or a protected host above, renders nothing.
   */
  import { SURFACE_KINDS, type ExtensionContext, type Surface } from '../extensions';
  import { extensionContext, extensions } from '../state/extensions.svelte';
  import ExtensionActions from './ExtensionActions.svelte';
  import ExtensionSetting from './ExtensionSetting.svelte';
  import ExtensionWidgets from './ExtensionWidgets.svelte';

  const {
    target,
    position,
    context,
  }: {
    target: Surface;
    position: 'before' | 'after';
    context?: (() => ExtensionContext) | null;
  } = $props();

  const inherited = extensionContext();
  const kinds = $derived(SURFACE_KINDS.get(target));
  const source = $derived(inherited && context !== null ? (context ?? inherited) : null);
  const current = $derived(source && extensions.targets(target) ? source() : null);
  const content = $derived(current ? extensions.content(target, position, current) : []);
  const hasActions = $derived(
    current && position === 'after' && kinds?.has('actions')
      ? extensions.actions(target, current).length > 0
      : false,
  );
  const settingGroups = $derived.by(() => {
    if (!current || position !== 'after' || !kinds?.has('settings')) return [];
    const settings = extensions.settings(target, current);
    return [...new Set(settings.map((s) => s.plugin))].map((plugin) => ({
      plugin,
      name: settings.find((s) => s.plugin === plugin)?.packageName ?? plugin,
      settings: settings.filter((s) => s.plugin === plugin),
    }));
  });
</script>

{#if current}
  {#each content as contribution (contribution.key)}
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
  {#if hasActions}
    <!-- In a row of its own: a bare button in a column flexbox stretches to the column's width. -->
    <div class="o-row"><ExtensionActions {target} context={source} /></div>
  {/if}
{/if}
