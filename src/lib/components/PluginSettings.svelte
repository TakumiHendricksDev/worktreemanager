<script lang="ts">
  /**
   * Settings → Plugins: built-in features and installed plugins, Workbench defaults, and Develop.
   *
   * This section is the way out of every plugin problem, which is why Settings itself is never
   * offered to plugin hooks and why this section is never a region a plugin can replace. Install,
   * update and roll back all go through a review of the exact snapshot and what it may run;
   * nothing here runs package code while preparing it. Plugin recovery and safe mode turn user
   * plugins off without uninstalling them.
   *
   * This file is the frame: the section tabs, the banners about the registry as a whole, the
   * status line, and the dialogs that outlive the row that opened them. Each tab is its own
   * component (`PluginList` and `PluginDetail`, `PluginWorkbenchDefaults`, `PluginDevelop`), and
   * every dialog and page owns its own failure, so an error is shown once, where it happened.
   *
   * # Linking here
   *
   * `settingsView.show('plugins', focus)` opens a tab when `focus` names one (`defaults` is what
   * New-session defaults… uses) or selects a plugin when it names one. The focus is consumed, so the
   * same link works again after the user has moved on.
   */
  import { tick, untrack } from 'svelte';
  import {
    type PluginInstalled,
    type PluginReviewStart,
    type PluginRow,
    type PluginSourceKind,
  } from '../plugins';
  import { plugins } from '../state/plugins.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import TextInput from './ui/TextInput.svelte';
  import ConfirmPluginAction from './ConfirmPluginAction.svelte';
  import PluginAuthoring from './PluginAuthoring.svelte';
  import PluginDetail from './PluginDetail.svelte';
  import PluginDevelop from './PluginDevelop.svelte';
  import PluginList from './PluginList.svelte';
  import PluginReviewDialog from './PluginReviewDialog.svelte';
  import PluginWorkbenchDefaults from './PluginWorkbenchDefaults.svelte';

  type Tab = 'installed' | 'defaults' | 'develop';
  const TABS: { id: Tab; name: string }[] = [
    { id: 'installed', name: 'Installed' },
    { id: 'defaults', name: 'Workbench defaults' },
    { id: 'develop', name: 'Develop a plugin' },
  ];
  type SourceFilter = 'all' | 'builtin' | PluginSourceKind;
  type Confirming =
    { action: 'uninstall' | 'rollback'; row: PluginRow } | { action: 'recover' };

  let authoring: PluginAuthoring;
  let tab = $state<Tab>('installed');
  let selected = $state('wtm.native-cli');
  let query = $state('');
  let sourceFilter = $state<SourceFilter>('all');
  /** The last change that worked, for the status line. */
  let notice = $state('');
  let reviewing = $state<PluginReviewStart | null>(null);
  let confirming = $state<Confirming | null>(null);
  /** Recovery ran in this window, which is why safe mode is on if it is. */
  let recovered = $state(false);
  let tabList = $state<HTMLElement | null>(null);

  const rows = $derived(
    plugins.rows.filter(
      (p) =>
        p.name.toLowerCase().includes(query.toLowerCase()) &&
        (sourceFilter === 'all' ||
          (sourceFilter === 'builtin'
            ? !p.installed
            : p.installed?.current.kind === sourceFilter)),
    ),
  );
  const current = $derived(plugins.rows.find((p) => p.id === selected));

  $effect(() => {
    const focus = settingsView.focus;
    if (!focus) return;
    untrack(() => {
      const named = TABS.find((t) => t.id === focus);
      if (named) show(named.id);
      else if (plugins.rows.some((p) => p.id === focus)) {
        show('installed');
        selected = focus;
      }
    });
    settingsView.focus = null;
  });

  function show(next: Tab) {
    if (next !== tab) notice = '';
    tab = next;
  }

  function onTabKey(event: KeyboardEvent, id: Tab) {
    const at = TABS.findIndex((t) => t.id === id);
    let next = at;
    if (event.key === 'ArrowRight') next = (at + 1) % TABS.length;
    else if (event.key === 'ArrowLeft') next = (at - 1 + TABS.length) % TABS.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = TABS.length - 1;
    else return;
    event.preventDefault();
    const target = TABS[next];
    if (target) show(target.id);
    void tick().then(() =>
      tabList?.querySelector<HTMLElement>('[aria-selected="true"]')?.focus(),
    );
  }

  function create() {
    reviewing = null;
    authoring.add();
  }

  function addFrom(kind: PluginSourceKind) {
    reviewing = { step: 'source', kind, update: null };
  }

  function update(installed: PluginInstalled) {
    reviewing = { step: 'source', kind: installed.current.kind, update: installed };
  }

  function installed(id: string, message: string) {
    reviewing = null;
    show('installed');
    if (!rows.some((r) => r.id === id)) {
      query = '';
      sourceFilter = 'all';
    }
    selected = id;
    notice = message;
    void authoring.refresh();
  }

  /**
   * The row to show once `id` is gone: the next one, the one before, or the first built-in, with
   * the search cleared when that is the only way to see it. Decided before the removal, while the
   * list still says where `id` was; applied after it, so a refused uninstall moves nothing.
   */
  function after(id: string): () => void {
    const visible = rows.map((r) => r.id);
    const at = visible.indexOf(id);
    const rest = visible.filter((r) => r !== id);
    const next = rest[Math.min(Math.max(at, 0), rest.length - 1)];
    return () => {
      if (!next) {
        query = '';
        sourceFilter = 'all';
      }
      selected = next ?? plugins.state.builtins[0]?.id ?? '';
    };
  }
</script>

<div class="c-plugins">
  <div class="c-plugins__head">
    <div class="c-tabs" role="tablist" aria-label="Plugin sections" bind:this={tabList}>
      {#each TABS as item (item.id)}
        <button
          class="c-tabs__tab"
          class:is-active={tab === item.id}
          role="tab"
          id="plugin-tab-{item.id}"
          aria-selected={tab === item.id}
          aria-controls="plugin-panel"
          tabindex={tab === item.id ? 0 : -1}
          onclick={() => show(item.id)}
          onkeydown={(event) => onTabKey(event, item.id)}>{item.name}</button
        >
      {/each}
    </div>
    <div class="c-plugins__add">
      <Button variant="accent" onclick={create}>Add plugin…</Button>
    </div>
  </div>
  <PluginAuthoring
    bind:this={authoring}
    showAll={tab === 'develop'}
    onsource={addFrom}
    onreview={(review, draft) => {
      reviewing = { step: 'review', review, draft };
    }}
  />
  {#if plugins.state.problem}<Banner>{plugins.state.problem}</Banner>{/if}
  {#if plugins.state.safe_mode && !plugins.state.problem}<Banner variant="info"
      >{recovered ? 'Plugin recovery ran.' : 'wtm started in safe mode.'} User plugins are off
      for this run; changes you make here apply now and are saved for the next normal start.</Banner
    >{/if}
  <div
    class="c-plugins__body"
    id="plugin-panel"
    role="tabpanel"
    aria-labelledby="plugin-tab-{tab}"
  >
    {#if tab === 'installed'}
      <div class="c-plugins__toolbar">
        <TextInput
          bind:value={query}
          type="search"
          placeholder="Search plugins…"
          ariaLabel="Search plugins"
        />
        <select
          class="c-input"
          aria-label="Filter plugins by source"
          bind:value={sourceFilter}
          ><option value="all">All sources</option><option value="builtin">Built-in</option
          ><option value="git">Git</option><option value="local">Local folder</option
          ></select
        >
      </div>
      <div class="c-plugins__split">
        <PluginList {rows} bind:selected onselect={() => (notice = '')} />
        <section class="c-plugins__detail" aria-label={current?.name ?? 'Plugin'}>
          {#if current}
            {#key current.id}
              <PluginDetail
                row={current}
                onupdate={update}
                onrollback={(row) => (confirming = { action: 'rollback', row })}
                onuninstall={(row) => (confirming = { action: 'uninstall', row })}
                ondefaults={() => show('defaults')}
                onnotice={(message) => (notice = message)}
              />
            {/key}
          {/if}
        </section>
      </div>
    {:else if tab === 'defaults'}
      <PluginWorkbenchDefaults onnotice={(message) => (notice = message)} />
    {:else}
      <PluginDevelop oncreate={create} onlink={() => addFrom('local')} />
    {/if}
  </div>
  <footer class="c-plugins__footer">
    <span role="status">{notice}</span><Button
      variant="quiet"
      size="sm"
      onclick={() => (confirming = { action: 'recover' })}>Plugin recovery…</Button
    >
  </footer>
</div>

{#if reviewing}
  <PluginReviewDialog
    start={reviewing}
    oncreate={create}
    onrefine={(id) => authoring.resume(id)}
    oninstalled={installed}
    onclose={() => (reviewing = null)}
  />
{/if}

{#if confirming?.action === 'uninstall'}
  {@const row = confirming.row}
  {@const kind = row.installed?.current.kind}
  <ConfirmPluginAction
    title={`Uninstall ${row.name}?`}
    confirm="Uninstall"
    working="Uninstalling…"
    action={async () => {
      const reselect = after(row.id);
      await plugins.uninstall(row.id);
      reselect();
    }}
    ondone={() => {
      // Before `confirming` is cleared: `row` is derived from it, and reads null after.
      notice = `Uninstalled ${row.name}. Its snapshots and saved data are kept.`;
      confirming = null;
    }}
    onclose={() => (confirming = null)}
  >
    <p>
      wtm closes {row.name}’s panels and views, stops its runtime and lets go of its
      sessions, then removes it from this list. Any region it filled shows its built-in view
      again.
    </p>
    <p>
      Its reviewed snapshots and saved data stay in ~/.config/wtm/plugins/{kind === 'local'
        ? ', and its source folder is left as it is'
        : ''}. Sessions on other hosts keep running.
    </p>
  </ConfirmPluginAction>
{:else if confirming?.action === 'rollback'}
  {@const row = confirming.row}
  {@const previous = row.installed?.previous}
  <ConfirmPluginAction
    title={`Roll back ${row.name}?`}
    confirm="Roll back"
    working="Rolling back…"
    action={() => plugins.rollback(row.id)}
    ondone={() => {
      notice = `Rolled ${row.name} back to ${previous?.manifest.version ?? 'its previous version'}.`;
      confirming = null;
    }}
    onclose={() => (confirming = null)}
  >
    <p>
      Go back to {previous?.manifest.version ?? 'the previous version'}, the one installed
      before
      {row.version}. wtm first checks that its files still match the digest you reviewed,
      and the version you have now becomes the one you can roll back to.
    </p>
    <p>
      Its runtime restarts on the older version. wtm can’t roll back while one of the
      plugin’s commands is running or one of its sessions is attached.
    </p>
  </ConfirmPluginAction>
{:else if confirming?.action === 'recover'}
  <ConfirmPluginAction
    title="Plugin recovery"
    confirm="Turn off user plugins"
    working="Recovering…"
    action={() => plugins.recover()}
    ondone={() => {
      confirming = null;
      recovered = true;
      notice =
        'Recovery finished. User plugins are off and every region shows wtm’s own view.';
    }}
    onclose={() => (confirming = null)}
  >
    <p>Recovery turns off every plugin you installed and puts wtm’s own parts back:</p>
    <ul>
      <li>Every region shows its built-in view again, including any you hid.</li>
      <li>
        Home, WTM Chat and Native CLI are turned back on, and new sessions open in WTM Chat.
      </li>
      <li>
        Choices that picked a plugin’s style or replacement are cleared. Choices of wtm’s
        default stay.
      </li>
      <li>
        Plugin panels and views close, plugin runtimes stop and their attached sessions are
        let go. Sessions on other hosts keep running.
      </li>
    </ul>
    <p>
      Plugins stay installed, with their snapshots, settings and saved data, so you can turn
      each one back on here. Until wtm restarts it stays in safe mode, which keeps a user
      plugin off unless you turn it on.
    </p>
    {#if plugins.state.problem}<p>
        The unreadable plugin registry is kept beside the new one in ~/.config/wtm/plugins/,
        as registry-corrupt-….json, for you to inspect.
      </p>{/if}
    <p class="c-note">
      If a plugin stops wtm from opening or hides this screen, quit wtm and start it in safe
      mode from Terminal: <code>open -na "Worktree Manager" --args --safe-mode</code>. User
      plugins stay off for that run.
    </p>
  </ConfirmPluginAction>
{/if}
