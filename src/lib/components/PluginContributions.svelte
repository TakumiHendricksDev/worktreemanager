<script lang="ts">
  /**
   * A package's in-feature contributions: what it adds inside wtm's own screens.
   *
   * For an installed package: each contribution's switch and order, the worktrees it applies in,
   * its settings, and every choice it takes part in. A replacement never applies until it is chosen
   * here, and a style waits for a choice only when another package styles the same part, so the
   * select shows wtm default until the user picks. Style choices and replacement choices are
   * separate entries, because one key for both meant a package shipping a style and a replacement
   * for the same target had one select deciding two unrelated things.
   *
   * With `review` it shows what a package declares before it is installed, read-only.
   *
   * # Order
   *
   * The registry orders contributions only against others in the same place (`group` in
   * `contributionRows`), so ↑ and ↓ move a row among those and nothing else, and the move is merged
   * into the saved order (`mergeOrder`) because `{ kind: 'order' }` replaces all of it. Moving
   * within this package's list alone, or rebuilding the list from active contributions, used to
   * drop every other package's and every other worktree's saved positions, and could swap a style
   * with an unrelated hook. A switched-off row has no position, so its arrows are off.
   *
   * # Controls that the host can refuse
   *
   * Every control here is controlled by the registry, and Svelte only writes a checkbox's `checked`
   * or a select's `value` when the bound value changes. A refused change never changed it, so the
   * control kept showing what the user clicked. `held` shows the clicked value while the change is
   * in flight and lets go afterwards, which is a real change either way and puts the control back
   * to whatever the registry now says.
   */
  import { emptyExtensions, type ExtensionContributions } from '../extensions';
  import {
    contributionRows,
    mergeOrder,
    orderBy,
    surfaceTitle,
    type ContributionRow,
  } from '../plugins';
  import { extensions } from '../state/extensions.svelte';
  import { plugins, PluginTask } from '../state/plugins.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Field from './ui/Field.svelte';
  import Banner from './ui/Banner.svelte';
  import ExtensionSetting from './ExtensionSetting.svelte';

  const {
    plugin,
    review,
    level = 5,
  }: {
    /** An installed package, whose contributions can be configured. */
    plugin?: string;
    /** A package under review, shown read-only. */
    review?: ExtensionContributions;
    /** The heading level of the subsections, one below whatever this sits under. */
    level?: 4 | 5;
  } = $props();

  const task = new PluginTask();
  let settingScope = $state('');
  /** Checkbox states and select values shown while a change is in flight. See the header. */
  const checks = $state<Record<string, boolean>>({});
  const values = $state<Record<string, string>>({});

  const manifest = $derived(
    plugin ? plugins.state.registry.installed[plugin]?.current.manifest : undefined,
  );
  const declared = $derived(review ?? manifest?.extensions ?? emptyExtensions);
  const editable = $derived(!!plugin && !review);
  const preferences = $derived(
    plugin ? plugins.state.registry.extensions[plugin] : undefined,
  );
  const context = $derived(extensions.context());
  const order = $derived(orderBy(plugins.state.registry.extension_order ?? []));
  const rows = $derived(
    contributionRows(plugin ?? 'review', declared).sort((a, b) => order(a.key, b.key)),
  );
  /** Every contribution that applies somewhere, by group, in the order the registry applies them. */
  const groups = $derived.by(() => {
    const out = new Map<string, string[]>();
    for (const installed of Object.values(plugins.state.registry.installed)) {
      const id = installed.current.manifest.id;
      if (!plugins.enabled(id)) continue;
      const off = plugins.state.registry.extensions[id]?.disabled ?? [];
      for (const row of contributionRows(id, installed.current.manifest.extensions)) {
        if (off.includes(row.id)) continue;
        const list = out.get(row.group);
        if (list) list.push(row.key);
        else out.set(row.group, [row.key]);
      }
    }
    for (const list of out.values()) list.sort(order);
    return out;
  });
  /** Every choice this package takes part in. See the header for why styles have their own. */
  const conflicts = $derived(
    extensions.choices.filter((choice) =>
      choice.candidates.some((c) => c.plugin === plugin),
    ),
  );
  const scoped = $derived(preferences?.worktrees ?? []);
  const heading = $derived(`h${level}`);

  function on(row: ContributionRow): boolean {
    return !!plugin && plugins.enabled(plugin) && !preferences?.disabled.includes(row.id);
  }

  /** Save one preference, showing `shown` in the control named `hold` until the answer. */
  async function configure(
    owner: string,
    value: unknown,
    hold?: { key: string; shown: boolean | string },
  ) {
    if (typeof hold?.shown === 'boolean') checks[hold.key] = hold.shown;
    else if (hold) values[hold.key] = hold.shown;
    await task.run(() => extensions.configure(owner, value));
    if (hold) {
      delete checks[hold.key];
      delete values[hold.key];
    }
  }

  function change(value: unknown, hold?: { key: string; shown: boolean | string }) {
    if (plugin) void configure(plugin, value, hold);
  }

  /** A choice is saved by the package that owns the chosen candidate, or by this one for wtm's. */
  function choose(target: string, key: string) {
    const owner = conflicts
      .find((choice) => choice.key === target)
      ?.candidates.find((c) => c.key === key);
    const by = owner?.plugin ?? plugin;
    if (!by) return;
    void configure(
      by,
      { kind: 'choice', target, id: owner ? owner.value.id : 'builtin' },
      { key: `choice:${target}`, shown: key },
    );
  }

  function chosen(target: string, candidates: { key: string }[]): string {
    return (
      plugins.state.registry.extension_choices[target] ??
      (target.startsWith('style:') && candidates.length === 1
        ? (candidates[0]?.key ?? 'builtin')
        : 'builtin')
    );
  }

  function choiceLabel(key: string): string {
    const part = key.startsWith('style:') ? key.slice('style:'.length) : null;
    return part === null ? `Replacement for ${key}` : `Style for ${surfaceTitle(part)}`;
  }

  /** Where a row stands among what competes with it, or null when nothing does. */
  function standing(row: ContributionRow): { at: number; of: number } | null {
    const list = groups.get(row.group);
    if (!on(row) || !list || list.length < 2) return null;
    return { at: list.indexOf(row.key), of: list.length };
  }

  function move(row: ContributionRow, direction: -1 | 1) {
    const list = [...(groups.get(row.group) ?? [])];
    const at = list.indexOf(row.key);
    const next = at + direction;
    const other = list[next];
    if (at < 0 || other === undefined) return;
    list[next] = row.key;
    list[at] = other;
    void change({
      kind: 'order',
      order: mergeOrder(plugins.state.registry.extension_order ?? [], list),
    });
  }

  function scope(value: string) {
    void change(
      { kind: 'scope', worktrees: value === 'all' ? [] : [context.worktreeId] },
      { key: 'scope', shown: value },
    );
  }

  function include(worktree: string, enabled: boolean) {
    const ids = new Set(scoped);
    if (enabled) ids.add(worktree);
    else ids.delete(worktree);
    void change(
      { kind: 'scope', worktrees: [...ids] },
      { key: `worktree:${worktree}`, shown: enabled },
    );
  }
</script>

<div class="c-extension-manager">
  {#if editable && rows.length}
    <Field
      id="plugin-contribution-scope"
      label="Apply in"
      help="Where wtm applies these additions. It does not sandbox the plugin's own programs."
    >
      <select
        id="plugin-contribution-scope"
        class="c-input"
        disabled={task.busy}
        value={values.scope ?? (scoped.length ? 'selected' : 'all')}
        onchange={(e) => scope(e.currentTarget.value)}
      >
        <option value="all">All worktrees</option><option
          value="selected"
          disabled={!context.worktreeId}>Selected worktrees</option
        >
      </select>
    </Field>
    {#if scoped.length}
      {#each workspace.worktrees as worktree (worktree.id)}
        {@const checked = checks[`worktree:${worktree.id}`] ?? scoped.includes(worktree.id)}
        <Choice
          {checked}
          disabled={task.busy || (checked && scoped.length === 1)}
          onchange={(enabled) => include(worktree.id, enabled)}>{worktree.title}</Choice
        >
      {/each}
      <p class="c-note">
        The last selected worktree stays selected. Choose All worktrees to apply everywhere.
      </p>
    {/if}
  {/if}
  {#if rows.length}
    <svelte:element this={heading} class="c-section-heading"
      >Inside wtm's own screens</svelte:element
    >
    {#each rows as row (row.key)}
      {@const place = standing(row)}
      <div class="c-extension-manager__row">
        <div>
          {#if editable}<Choice
              disabled={task.busy}
              checked={checks[`enabled:${row.id}`] ??
                !preferences?.disabled.includes(row.id)}
              onchange={(enabled) =>
                void change(
                  { kind: 'enabled', id: row.id, enabled },
                  { key: `enabled:${row.id}`, shown: enabled },
                )}>{row.title}</Choice
            >{:else}<strong>{row.title}</strong>{/if}
          <p class="c-note">
            {row.kind} · {#if row.operation}<code>{row.place}</code
              >{:else}{row.place}{/if}{#if place}{` · position ${place.at + 1} of ${place.of}`}{/if}
          </p>
        </div>
        {#if editable}<div class="o-row">
            <Button
              size="sm"
              disabled={task.busy || !place || place.at === 0}
              ariaLabel={`Move ${row.title} earlier`}
              onclick={() => move(row, -1)}>↑</Button
            ><Button
              size="sm"
              disabled={task.busy || !place || place.at === place.of - 1}
              ariaLabel={`Move ${row.title} later`}
              onclick={() => move(row, 1)}>↓</Button
            >
          </div>{/if}
      </div>
    {/each}
  {:else}
    <p class="c-note">No additions inside wtm's own screens.</p>
  {/if}
  {#if editable && plugin}
    {#each conflicts as choice (choice.key)}
      <Field
        id={'conflict-' + choice.key}
        label={choiceLabel(choice.key)}
        help={choice.candidates.length > 1
          ? 'Several plugins target this. wtm keeps its own behaviour until you choose one.'
          : choice.key.startsWith('style:')
            ? 'Applies on its own while no other plugin styles this part.'
            : 'A replacement applies only once you choose it here.'}
      >
        <select
          id={'conflict-' + choice.key}
          class="c-input"
          disabled={task.busy}
          value={values[`choice:${choice.key}`] ?? chosen(choice.key, choice.candidates)}
          onchange={(e) => choose(choice.key, e.currentTarget.value)}
          ><option value="builtin">wtm default</option
          >{#each choice.candidates as c (c.key)}<option value={c.key}
              >{c.packageName} · {c.value.title}</option
            >{/each}</select
        >
      </Field>
    {/each}
    {#if declared.settings.length}
      <svelte:element this={heading} class="c-section-heading"
        >Plugin settings</svelte:element
      >
      <Field id="extension-setting-scope" label="Save settings for"
        ><select id="extension-setting-scope" class="c-input" bind:value={settingScope}
          ><option value="">All worktrees</option>{#if context.worktreeId}<option
              value={context.worktreeId}>This worktree</option
            >{/if}</select
        ></Field
      >
      {#each declared.settings as value (value.id)}<ExtensionSetting
          setting={{
            plugin,
            packageName: manifest?.name ?? plugin,
            key: `${plugin}:${value.id}`,
            value,
          }}
          {context}
          scope={settingScope}
        />{/each}
    {/if}
  {/if}
  {#if task.error}<Banner>{task.error}</Banner>{/if}
</div>
