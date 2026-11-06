<script lang="ts">
  import { catalog, emptyExtensions, type ExtensionContributions } from '../extensions';
  import { plugins } from '../state/plugins.svelte';
  import { extensions } from '../state/extensions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Field from './ui/Field.svelte';
  import TextInput from './ui/TextInput.svelte';
  import Banner from './ui/Banner.svelte';
  import ExtensionSetting from './ExtensionSetting.svelte';

  const {
    plugin,
    inventory = false,
    review,
  }: { plugin?: string; inventory?: boolean; review?: ExtensionContributions } = $props();
  let query = $state('');
  let busy = $state(false);
  let problem = $state('');
  let settingScope = $state('');
  const manifest = $derived(
    plugin ? plugins.state.registry.installed[plugin]?.current.manifest : undefined,
  );
  const declared = $derived(review ?? manifest?.extensions ?? emptyExtensions);
  const preferences = $derived(
    plugin ? plugins.state.registry.extensions[plugin] : undefined,
  );
  const context = $derived(extensions.context());
  const rows = $derived([
    ...declared.styles.map((value) => ({ kind: 'Style', target: value.target, value })),
    ...declared.actions.map((value) => ({
      kind: 'Action',
      target: value.replaces ?? value.target,
      value,
    })),
    ...declared.content.map((value) => ({
      kind: `Content · ${value.position}`,
      target: value.target,
      value,
    })),
    ...declared.settings.map((value) => ({
      kind: 'Setting',
      target: value.section,
      value,
    })),
    ...declared.hooks.map((value) => ({
      kind: `Hook · ${value.phase}`,
      target: value.operation,
      value,
    })),
  ]);
  const contracts = $derived(
    [
      ...catalog.surfaces.map((s) => ({
        id: s.id,
        description: s.reason,
        status: s.kinds.length ? s.kinds.join(', ') : 'Protected / infrastructure',
        version: s.version,
      })),
      ...catalog.events.map((e) => ({
        id: e.id,
        description: e.reason,
        status: 'event',
        version: e.version,
      })),
      ...catalog.operations.map((o) => ({
        id: o.id,
        description: o.reason,
        status: o.exposure,
        version: o.version,
      })),
    ].filter((c) =>
      `${c.id} ${c.description} ${c.status}`.toLowerCase().includes(query.toLowerCase()),
    ),
  );
  const conflicts = $derived.by(() => {
    const grouped = new Map<
      string,
      { key: string; title: string; plugin: string; id: string }[]
    >();
    for (const kind of ['styles', 'content', 'actions'] as const) {
      for (const c of extensions.contributions(kind, context)) {
        const v = c.value;
        const target =
          'replaces' in v
            ? v.replaces
            : 'position' in v
              ? v.position === 'replace'
                ? v.target
                : null
              : v.target;
        if (!target) continue;
        const list = grouped.get(target) ?? [];
        list.push({
          key: c.key,
          title: `${c.packageName} · ${v.title}`,
          plugin: c.plugin,
          id: v.id,
        });
        grouped.set(target, list);
      }
    }
    return [...grouped].filter(([, list]) => list.some((c) => c.plugin === plugin));
  });
  async function run(action: () => Promise<unknown>) {
    busy = true;
    problem = '';
    try {
      await action();
    } catch (e) {
      problem = errorMessage(e);
    } finally {
      busy = false;
    }
  }
  function change(value: unknown) {
    if (plugin) void run(() => extensions.configure(plugin, value));
  }
  function choose(target: string, key: string) {
    const owner = conflicts.find(([t]) => t === target)?.[1].find((c) => c.key === key);
    if (owner)
      void run(() =>
        extensions.configure(owner.plugin, { kind: 'choice', target, id: owner.id }),
      );
    else change({ kind: 'choice', target, id: 'builtin' });
  }
  function move(id: string, direction: number) {
    const all = ['styles', 'actions', 'content', 'settings', 'hooks'].flatMap((kind) =>
      extensions.contributions(kind as 'styles', context).map((c) => c.key),
    );
    const at = all.indexOf(`${plugin}:${id}`),
      next = at + direction;
    if (at < 0 || next < 0 || next >= all.length) return;
    const first = all[at],
      second = all[next];
    if (first === undefined || second === undefined) return;
    all[at] = second;
    all[next] = first;
    change({ kind: 'order', order: all });
  }
</script>

<div class="c-extension-manager">
  {#if inventory}
    <h3>Public contract inventory</h3>
    <p>
      API 2 contracts used by WTM and plugins. Protected entries retain host enforcement.
      Provider operations can report unsupported capabilities.
    </p>
    <TextInput
      type="search"
      bind:value={query}
      ariaLabel="Search public contracts"
      placeholder="Chat, workspace, panel, settings…"
    />
    {#each contracts as c (c.id)}<div class="c-extension-manager__row">
        <div>
          <code>{c.id}</code>
          <p class="c-extension-manager__meta">{c.description}</p>
        </div>
        <span class="c-extension-manager__meta">v{c.version} · {c.status}</span>
      </div>{/each}
  {:else}
    <h3>Contributions</h3>
    {#if plugin && !review && rows.length}
      <Field
        id="plugin-contribution-scope"
        label="Apply in"
        help="Scope controls where WTM applies contributions. It does not sandbox native plugin code."
      >
        <select
          id="plugin-contribution-scope"
          class="c-input"
          disabled={busy}
          value={preferences?.worktrees.length ? 'selected' : 'all'}
          onchange={(e) =>
            change({
              kind: 'scope',
              worktrees: e.currentTarget.value === 'all' ? [] : [context.worktreeId],
            })}
        >
          <option value="all">All workspaces</option><option
            value="selected"
            disabled={!context.worktreeId}>Selected workspaces</option
          >
        </select>
      </Field>
      {#if preferences?.worktrees.length}
        {#each workspace.worktrees as worktree (worktree.id)}
          <Choice
            checked={preferences.worktrees.includes(worktree.id)}
            disabled={busy}
            onchange={(enabled) => {
              const ids = new Set(preferences?.worktrees);
              if (enabled) ids.add(worktree.id);
              else ids.delete(worktree.id);
              if (ids.size) change({ kind: 'scope', worktrees: [...ids] });
            }}>{worktree.title}</Choice
          >
        {/each}
        <p class="c-extension-manager__meta">
          Keep at least one workspace selected, or choose All workspaces.
        </p>
      {/if}
    {/if}
    {#each rows as row (row.value.id)}
      <div class="c-extension-manager__row">
        <div>
          {#if plugin && !review}<Choice
              disabled={busy}
              checked={!preferences?.disabled.includes(row.value.id)}
              onchange={(enabled) => change({ kind: 'enabled', id: row.value.id, enabled })}
              >{row.value.title}</Choice
            >{:else}<strong>{row.value.title}</strong>{/if}
          <p class="c-extension-manager__meta">{row.kind} · <code>{row.target}</code></p>
        </div>
        {#if plugin && !review}<div>
            <Button
              size="sm"
              disabled={busy}
              ariaLabel={`Move ${row.value.title} earlier`}
              onclick={() => move(row.value.id, -1)}>↑</Button
            ><Button
              size="sm"
              disabled={busy}
              ariaLabel={`Move ${row.value.title} later`}
              onclick={() => move(row.value.id, 1)}>↓</Button
            >
          </div>{/if}
      </div>
    {/each}
    {#if !rows.length}<p class="c-extension-manager__meta">
        No in-feature contributions declared.
      </p>{/if}
    {#if declared.events?.length}<h4>Event subscriptions</h4>
      <p class="c-extension-manager__meta">{declared.events.join(', ')}</p>{/if}
    {#if declared.access.length}<h4>Declared host access</h4>
      <p class="c-extension-manager__meta">{declared.access.join(', ')}</p>{/if}
    {#if declared.runtime.length}<Banner variant="warn"
        >Runs native code as your user: {declared.runtime.join(' ')}. Native code can access
        files and the network.</Banner
      >{/if}
    {#if plugin && !review}
      {#each conflicts as [target, candidates] (target)}
        <Field
          id={'conflict-' + target}
          label={`Use for ${target}`}
          help={candidates.length > 1
            ? 'Several plugins target this part. WTM keeps its built-in behavior until you choose a winner.'
            : 'An explicit choice persists across reloads.'}
        >
          <select
            id={'conflict-' + target}
            class="c-input"
            disabled={busy}
            value={plugins.state.registry.extension_choices[target] ??
              (candidates.length === 1 ? candidates[0]?.key : 'builtin')}
            onchange={(e) => choose(target, e.currentTarget.value)}
            ><option value="builtin">WTM default</option
            >{#each candidates as c (c.key)}<option value={c.key}>{c.title}</option
              >{/each}</select
          >
        </Field>
      {/each}
      {#if declared.settings.length}
        <h4>Plugin settings</h4>
        <Field id="extension-setting-scope" label="Save settings for"
          ><select id="extension-setting-scope" class="c-input" bind:value={settingScope}
            ><option value="">All workspaces</option>{#if context.worktreeId}<option
                value={context.worktreeId}>This workspace</option
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
      {#if extensions.problems[plugin]}<Banner variant="error"
          >{extensions.problems[plugin]}</Banner
        >{/if}
      {#if declared.runtime.length}<Button
          size="sm"
          disabled={busy || !plugins.enabled(plugin)}
          onclick={() =>
            run(async () => {
              await commands.extensionRestart(plugin);
              delete extensions.problems[plugin];
            })}>Restart runtime</Button
        >{/if}
    {/if}
  {/if}
  {#if problem}<Banner variant="error">{problem}</Banner>{/if}
</div>
