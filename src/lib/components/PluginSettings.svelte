<script lang="ts">
  import { setContext as setExtensionContext } from 'svelte';
  import { EXTENSION_PROTECTED } from '../state/extensions.svelte';
  setExtensionContext(EXTENSION_PROTECTED, true);
  import { onDestroy } from 'svelte';
  import { open } from '@tauri-apps/plugin-dialog';
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import {
    PLUGIN_SLOTS,
    type PluginReview,
    type PluginSlot,
    type PluginBinding,
    type PluginDraftView,
  } from '../plugins';
  import { plugins } from '../state/plugins.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import { sessions } from '../state/sessions.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import TextInput from './ui/TextInput.svelte';
  import Field from './ui/Field.svelte';
  import Banner from './ui/Banner.svelte';
  import Dialog from './ui/Dialog.svelte';
  import PluginContributions from './PluginContributions.svelte';
  import PluginSessions from './PluginSessions.svelte';
  import PluginAuthoring from './PluginAuthoring.svelte';

  let authoring: PluginAuthoring;
  let reviewDraft = $state<PluginDraftView | null>(null);
  let tab = $state<'installed' | 'defaults' | 'develop'>('installed');
  let selected = $state('wtm.native-cli');
  let query = $state('');
  let sourceFilter = $state('all');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let dialog = $state<
    'add' | 'review' | 'manifest' | 'output' | 'remove' | 'recovery' | 'rollback' | null
  >(null);
  let source = $state('');
  let kind = $state('local');
  let revision = $state('main');
  let review = $state<PluginReview | null>(null);
  let gone = false;
  let listElement = $state<HTMLElement | null>(null);
  let output = $state('');
  let backend = $state<{ plugin: string; id: string; title: string } | null>(null);
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
    void selected;
    void rows;
    const frame = requestAnimationFrame(() =>
      listElement?.querySelector('.is-selected')?.scrollIntoView({ block: 'nearest' }),
    );
    return () => cancelAnimationFrame(frame);
  });
  const manifest = $derived(current?.installed?.current.manifest);
  const projectId = $derived(workspace.activeProjectId);
  const worktreeId = $derived(workspace.selectedWorktreeId);
  const starter = `schema = 1\napi = 1\nid = "example.project-tools"\nname = "Project Tools"\nversion = "0.1.0"\n\n[[commands]]\nid = "summary"\ntitle = "Show project summary"\ncommand = ["git", "status", "--short"]`;

  async function run(action: () => Promise<unknown>) {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await action();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
  async function close() {
    if (busy) return;
    if (review) {
      const token = review.token;
      review = null;
      await commands.pluginCancel(token).catch(() => {});
    }
    dialog = null;
  }
  onDestroy(() => {
    gone = true;
    if (review) void commands.pluginCancel(review.token).catch(() => {});
  });
  function add() {
    dialog = null;
    authoring.add();
  }
  function sourceAdd(next: 'local' | 'git') {
    reviewDraft = null;
    source = '';
    kind = next;
    revision = 'main';
    dialog = 'add';
  }
  async function browse() {
    await run(async () => {
      const path = await open({
        directory: true,
        multiple: false,
        title: 'Choose a plugin folder',
      });
      if (typeof path === 'string' && !gone) source = path;
    });
  }
  async function prepare() {
    await run(async () => {
      const prepared = await commands.pluginPrepare(source, kind, revision);
      if (gone) {
        await commands.pluginCancel(prepared.token);
        return;
      }
      review = prepared;
      dialog = 'review';
    });
  }
  function update() {
    if (!current?.installed) return;
    reviewDraft = null;
    source = current.installed.current.source;
    kind = current.installed.current.kind;
    revision = current.installed.current.revision;
    dialog = 'add';
  }
  async function install() {
    if (!review) return;
    const pending = review;
    await run(async () => {
      await plugins.change('install', pending.token);
      selected = pending.package.manifest.id;
      review = null;
      dialog = null;
      reviewDraft = null;
      await authoring.refresh();
    });
  }
  async function contribution(id: string, panel: boolean) {
    if (!current || !projectId || !worktreeId) return;
    const plugin = current.id;
    await run(async () => {
      if (panel) {
        await commands.pluginPanel(projectId, worktreeId, plugin, id);
        settingsView.close();
      } else {
        output = await commands.pluginRun(projectId, worktreeId, plugin, id);
        dialog = 'output';
      }
    });
  }
  function choiceFor(slot: PluginSlot): string {
    const b = plugins.binding(slot);
    return b.kind === 'plugin' ? `${b.plugin}:${b.view}` : b.kind;
  }
  function chooseSlot(slot: PluginSlot, value: string) {
    const [plugin = '', view = ''] = value.split(':');
    const binding: PluginBinding =
      value === 'builtin' || value === 'hidden'
        ? { kind: value }
        : { kind: 'plugin', plugin, view };
    void run(() => plugins.bind(slot, binding));
  }
</script>

<div class="c-plugins">
  <div class="c-plugins__toolbar">
    <div class="c-plugins__tabs" aria-label="Plugin sections">
      {#each [{ id: 'installed', name: 'Installed' }, { id: 'defaults', name: 'Workbench defaults' }, { id: 'develop', name: 'Develop a plugin' }] as item}
        <Button
          variant={tab === item.id ? 'neutral' : 'quiet'}
          ariaPressed={tab === item.id}
          onclick={() => {
            tab = item.id as typeof tab;
          }}>{item.name}</Button
        >
      {/each}
    </div>
    <Button variant="accent" onclick={add} disabled={busy}>Add plugin…</Button>
  </div>
  <PluginAuthoring
    bind:this={authoring}
    showAll={tab === 'develop'}
    onsource={sourceAdd}
    onreview={(prepared, draft) => {
      review = prepared;
      reviewDraft = draft;
      error = null;
      dialog = 'review';
    }}
  />
  {#if error}<Banner>{error}</Banner>{/if}
  {#if plugins.state.problem}<Banner>{plugins.state.problem}</Banner>{/if}
  {#if plugins.state.registry.safe_mode}<Banner variant="info"
      >User-installed plugins are disabled. Enable individual plugins when you are ready.</Banner
    >{/if}
  <div class="c-plugins__body">
    {#if tab === 'installed'}
      <div class="c-plugins__toolbar">
        <TextInput
          bind:value={query}
          type="search"
          placeholder="Search installed plugins…"
          ariaLabel="Search installed plugins"
        />
        <select class="c-input" aria-label="Filter plugins" bind:value={sourceFilter}
          ><option value="all">All sources</option><option value="builtin">Built-in</option
          ><option value="git">Git</option><option value="local">Local</option></select
        >
      </div>
      <div class="c-plugins__split">
        <div class="c-plugins__list" bind:this={listElement}>
          {#each rows as p (p.id)}
            <button
              class="c-plugins__item"
              class:is-selected={selected === p.id}
              aria-pressed={selected === p.id}
              onclick={() => {
                selected = p.id;
              }}
            >
              <span class="c-plugins__row"
                ><strong>{p.name}</strong><span
                  class="c-plugins__status"
                  class:is-enabled={p.enabled}>{p.enabled ? 'On' : 'Off'}</span
                ></span
              ><span class="c-plugins__muted">{p.source}</span>
            </button>
          {:else}<p class="c-plugins__empty">No plugins match this search.</p>{/each}
        </div>
        <section class="c-plugins__detail">
          {#if current}
            <div class="c-plugins__row">
              <div>
                <h2 class="c-plugins__title">
                  {current.name} <span class="c-badge">{current.version}</span>
                </h2>
                <p class="c-plugins__muted">{current.id}</p>
              </div>
              <Choice
                checked={current.enabled}
                disabled={busy}
                onchange={(enabled) =>
                  void run(() => plugins.change('enable', current.id, enabled))}
                >Enabled</Choice
              >
            </div>
            <p class="c-plugins__description">{current.description}</p>
            {#if current.id === 'wtm.native-cli'}
              <p class="c-plugins__note">
                Use the original CLI experience, including its own mods. WTM manages the
                pane; the CLI manages the conversation.
              </p>
              <div class="c-plugins__section c-plugins__row">
                <div>
                  Default for new local sessions
                  <p class="c-plugins__muted">
                    Currently {plugins.state.registry.experience === 'wtm.chat'
                      ? 'WTM Chat'
                      : 'Native CLI'}
                  </p>
                </div>
                <Button
                  onclick={() => {
                    tab = 'defaults';
                  }}>Choose experience…</Button
                >
              </div>
            {:else if current.id === 'wtm.home'}
              <p class="c-plugins__note">
                Disabling Home removes its entry points and prevents it from loading. Close
                its running sessions first. Saved conversations and worktrees are kept.
              </p>
            {/if}
            <div class="c-plugins__section">
              <h3 class="c-section-heading">What this plugin adds</h3>
              {#if manifest}
                <PluginContributions plugin={manifest.id} />
                {#each manifest.terminals as terminal}<div class="c-plugins__contribution">
                    <span
                      >{terminal.title}<small class="c-plugins__muted"
                        >Terminal profile</small
                      ></span
                    ><Button
                      size="sm"
                      disabled={!current.enabled || !projectId || !worktreeId}
                      onclick={() => {
                        if (projectId && worktreeId) {
                          void sessions.openPluginTerminal(
                            projectId,
                            worktreeId,
                            current.id,
                            terminal.id,
                            terminal.title,
                          );
                          settingsView.close();
                        }
                      }}>Open terminal</Button
                    >
                  </div>{/each}
                {#each manifest.views as v}<div class="c-plugins__contribution">
                    <span
                      >{v.title}<small class="c-plugins__muted"
                        >Replacement view · {v.slots
                          .map((s) => PLUGIN_SLOTS[s])
                          .join(', ')}</small
                      ></span
                    ><Button
                      size="sm"
                      disabled={!current.enabled}
                      onclick={() => (tab = 'defaults')}>Choose region…</Button
                    >
                  </div>{/each}
                {#each manifest.palettes as palette}<div class="c-plugins__contribution">
                    <span
                      >{palette.title}<small class="c-plugins__muted">Palette</small></span
                    ><Button
                      size="sm"
                      disabled={!current.enabled}
                      onclick={() => settingsView.show('appearance')}
                      >Choose palette…</Button
                    >
                  </div>{/each}
                {#each manifest.commands as command}<div class="c-plugins__contribution">
                    <span
                      >{command.title}<small class="c-plugins__muted">Command</small></span
                    ><Button
                      size="sm"
                      disabled={busy || !current.enabled || !worktreeId}
                      onclick={() => void contribution(command.id, false)}>Run</Button
                    >
                  </div>{/each}
                {#each manifest.panels as panel}<div class="c-plugins__contribution">
                    <span
                      >{panel.title}<small class="c-plugins__muted">Isolated panel</small
                      ></span
                    ><Button
                      size="sm"
                      disabled={busy || !current.enabled || !worktreeId}
                      onclick={() => void contribution(panel.id, true)}>Open</Button
                    >
                  </div>{/each}
                {#each manifest.session_backends as b}<div class="c-plugins__contribution">
                    <span
                      >{b.title}<small class="c-plugins__muted">Session backend</small
                      ></span
                    ><Button
                      size="sm"
                      disabled={!current.enabled}
                      onclick={() => {
                        backend = { plugin: current.id, id: b.id, title: b.title };
                      }}>Browse sessions…</Button
                    >
                  </div>{/each}
                {#if !worktreeId && (manifest.commands.length || manifest.panels.length)}<p
                    class="c-plugins__muted"
                  >
                    Select a worktree to use its commands and panels.
                  </p>{/if}
              {:else}<p>
                  {current.description}
                </p>{/if}
            </div>
            <div class="c-plugins__section">
              <h3 class="c-section-heading">Access &amp; source</h3>
              <p class="c-plugins__muted">
                {manifest &&
                (manifest.commands.length ||
                  manifest.session_backends.length ||
                  manifest.terminals.length ||
                  manifest.extensions.runtime.length)
                  ? 'Runs executable code as your user, with access to files and the network. This is not sandboxed.'
                  : manifest
                    ? 'Isolated local UI. Replacement views use the public Plugin API; no private app DOM or direct network access.'
                    : 'WTM-managed feature.'}
              </p>
              <dl class="c-plugins__facts">
                <dt>Source</dt>
                <dd>{current.installed?.current.source ?? 'Bundled with WTM'}</dd>
                <dt>Revision</dt>
                <dd>{current.installed?.current.revision ?? 'Follows your WTM release'}</dd>
                <dt>Compatibility</dt>
                <dd>Plugin API {manifest?.api ?? 1}</dd>
                <dt>Status</dt>
                <dd>{current.enabled ? 'Enabled' : 'Disabled'}</dd>
              </dl>
            </div>
            {#if current.installed}<div class="c-plugins__section c-plugins__actions">
                <Button onclick={update} disabled={busy}
                  >{current.installed.current.kind === 'git'
                    ? 'Review update…'
                    : 'Reload & review…'}</Button
                >{#if current.installed.previous}<Button
                    onclick={() => {
                      dialog = 'rollback';
                    }}>Roll back…</Button
                  >{/if}<Button
                  onclick={() => {
                    dialog = 'manifest';
                  }}>View manifest</Button
                ><Button
                  variant="quiet"
                  onclick={() => {
                    dialog = 'remove';
                  }}>Uninstall…</Button
                >
              </div>{/if}
          {/if}
        </section>
      </div>
    {:else if tab === 'defaults'}
      <div class="c-plugins__content">
        <h2 class="c-plugins__title">Choose how you work</h2>
        <p class="c-plugins__description">
          Choose where sessions run and how you interact with them independently.
        </p>
        <div class="c-plugins__section">
          <h3 class="c-section-heading">Session host</h3>
          <p>This computer</p>
          <p class="c-plugins__muted">
            Installed session backends expose their own hosts through Browse sessions.
            Attaching to an existing session keeps its host.
          </p>
        </div>
        <div class="c-plugins__section">
          <h3 class="c-section-heading">Local agent experience</h3>
          <div class="c-plugins__choices">
            {#each ['wtm.chat', 'wtm.native-cli'] as id}<button
                class="c-plugins__choice"
                class:is-selected={plugins.state.registry.experience === id}
                aria-pressed={plugins.state.registry.experience === id}
                disabled={busy || !plugins.enabled(id)}
                onclick={() => void run(() => plugins.change('experience', id))}
                ><strong>{id === 'wtm.chat' ? 'WTM Chat' : 'Native CLI'}</strong><span
                  class="c-plugins__muted"
                  >{id === 'wtm.chat'
                    ? 'WTM’s transcript, tools, approvals, and delegation.'
                    : 'The vendor’s terminal UI, commands, and permission prompts.'}</span
                ></button
              >{/each}
          </div>
        </div>
        <p class="c-plugins__note">
          Native CLI uses the installed claude or codex command directly. Structured WTM
          approvals, delegation, and turn status remain features of WTM Chat. Existing
          sessions keep their current experience.
        </p>
        <div class="c-plugins__section">
          <h3 class="c-section-heading">Feature replacements</h3>
          <p>Installing a plugin never silently replaces your selected experience.</p>
          {#each Object.entries(PLUGIN_SLOTS) as [key, label]}
            {@const slot = key as PluginSlot}
            <Field id={`plugin-slot-${slot}`} {label}>
              <select
                id={`plugin-slot-${slot}`}
                class="c-input"
                value={choiceFor(slot)}
                disabled={busy}
                onchange={(e) => chooseSlot(slot, e.currentTarget.value)}
              >
                <option value="builtin">Built-in</option><option value="hidden"
                  >Hidden</option
                >
                {#each Object.values(plugins.state.registry.installed) as p}
                  {#if plugins.enabled(p.current.manifest.id)}
                    {#each p.current.manifest.views.filter( (v) => v.slots.includes(slot) ) as v}
                      <option value={`${p.current.manifest.id}:${v.id}`}
                        >{p.current.manifest.name} · {v.title}</option
                      >
                    {/each}
                  {/if}
                {/each}
              </select>
            </Field>
          {/each}
          <p class="c-plugins__note">
            A replacement owns its region's UI. Settings, permission checks and recovery
            stay with WTM. Disabling or removing a replacement restores the built-in when
            enabled. Hiding a view preserves its data and running sessions.
          </p>
        </div>
      </div>
    {:else}
      <div class="c-plugins__content">
        <h2 class="c-plugins__title">Build a plugin with an agent</h2>
        <p class="c-plugins__description">
          Describe a customization and keep refining it in a dedicated Claude or Codex
          conversation.
        </p>
        <Button variant="accent" onclick={add}>Create with an agent…</Button>
        <h2 class="c-plugins__title c-plugins__section">A folder is a plugin</h2>
        <p class="c-plugins__description">
          Create wtm-plugin.toml in a folder. Add styles, actions, settings, content and
          behavior inside existing features, or declare commands, panels, replacement views
          and session backends. Use the public contracts below to choose what to customize.
        </p>
        <pre class="c-plugins__code">{starter}</pre>
        <PluginContributions inventory />
        <p class="c-plugins__muted">
          Start with the examples/plugins packages and docs/plugins.md in the WTM source.
          Session backends speak versioned JSON-RPC over stdin and stdout; your
          implementation can use SSH to reach a persistent session host.
        </p>
        <div class="c-plugins__section">
          <Button variant="accent" onclick={() => sourceAdd('local')}
            >Link development folder…</Button
          >
        </div>
        <p class="c-plugins__note">
          Validate → review access → enable → reload. WTM runs a reviewed snapshot, so
          editing a linked folder does not silently change trusted code.
        </p>
        <PluginContributions inventory />
      </div>
    {/if}
  </div>
  <footer class="c-plugins__footer">
    <span>Running sessions keep their current backend.</span><Button
      variant="quiet"
      size="sm"
      onclick={() => {
        dialog = 'recovery';
      }}>Plugin recovery…</Button
    >
  </footer>
</div>

{#if dialog}
  <Dialog
    title={dialog === 'add'
      ? 'Add plugin'
      : dialog === 'review'
        ? 'Review plugin'
        : dialog === 'recovery'
          ? 'Plugin recovery'
          : dialog === 'remove'
            ? 'Uninstall plugin?'
            : dialog === 'rollback'
              ? 'Roll back plugin?'
              : dialog === 'manifest'
                ? 'Manifest'
                : 'Command output'}
    onclose={() => void close()}
    closeDisabled={busy}
  >
    {#snippet body()}
      <div class="o-stack">
        {#if error}<Banner>{error}</Banner>{/if}
        {#if dialog === 'add'}
          <div class="c-plugins__tabs" aria-label="How to add a plugin">
            <Button size="sm" variant="quiet" disabled={busy} onclick={add}
              >Create with an agent</Button
            >
            <Button
              size="sm"
              variant={kind === 'local' ? 'neutral' : 'quiet'}
              ariaPressed={kind === 'local'}
              disabled={busy}
              onclick={() => {
                kind = 'local';
              }}>Local folder</Button
            >
            <Button
              size="sm"
              variant={kind === 'git' ? 'neutral' : 'quiet'}
              ariaPressed={kind === 'git'}
              disabled={busy}
              onclick={() => {
                kind = 'git';
              }}>Git repository</Button
            >
          </div>
          <Field id="plugin-kind" label="Source"
            ><select id="plugin-kind" class="c-input" bind:value={kind}
              ><option value="local">Local development folder</option><option value="git"
                >Git repository (HTTPS)</option
              ></select
            ></Field
          >
          <Field
            id="plugin-source"
            label={kind === 'git' ? 'Repository URL' : 'Absolute folder path'}
            ><TextInput
              id="plugin-source"
              bind:value={source}
              placeholder={kind === 'git'
                ? 'https://github.com/owner/plugin'
                : '/path/to/my-plugin'}
            /></Field
          >
          {#if kind === 'local'}<Button disabled={busy} onclick={() => void browse()}
              >Choose folder…</Button
            >{/if}
          {#if kind === 'git'}<Field
              id="plugin-revision"
              label="Tag, branch or commit"
              help="Review resolves this to an immutable commit. No automatic updates."
              ><TextInput id="plugin-revision" bind:value={revision} /></Field
            >{/if}
          <p class="c-field__help">Nothing in the package runs before review.</p>
        {:else if dialog === 'review' && review}
          {#if reviewDraft}<p class="c-plugins__muted">
              Created with {reviewDraft.provider === 'claude' ? 'Claude' : 'Codex'} · Manifest
              and packaged files pass validation.
            </p>{/if}
          <PluginContributions review={review.package.manifest.extensions} />
          <h3>{review.package.manifest.name} · {review.package.manifest.version}</h3>
          <p>{review.package.manifest.description}</p>
          <dl class="c-plugins__facts">
            <dt>Source</dt>
            <dd>{review.package.source}</dd>
            <dt>Revision</dt>
            <dd>{review.package.revision}</dd>
            <dt>Digest</dt>
            <dd>{review.package.digest}</dd>
          </dl>
          {#if review.package.manifest.commands.length || review.package.manifest.session_backends.length || review.package.manifest.terminals.length || review.package.manifest.extensions.runtime.length}<Banner
              variant="warn"
              >This plugin runs code as your user. It can access your files and the network.
              Install only code you trust.</Banner
            >{/if}
          {#each [...review.package.manifest.commands, ...review.package.manifest.session_backends, ...review.package.manifest.terminals] as c}<p
            >
              <strong>{c.title}</strong>
            </p>
            <pre class="c-plugins__code">{JSON.stringify(c.command)}</pre>{/each}
          {#each review.package.manifest.panels as p}<p>
              Panel: {p.title} · {p.file}
            </p>{/each}
          {#if reviewDraft}<p class="c-plugins__note">
              Installation saves this reviewed snapshot. Later edits need a new review.
              Validation checks packaging, not whether the plugin behaves correctly. Your
              selected feature replacements stay unchanged.
            </p>{/if}
          {#each review.package.manifest.views as v}<p>
              View: {v.title} · {v.slots.map((s) => PLUGIN_SLOTS[s]).join(', ')}
            </p>{/each}
        {:else if dialog === 'manifest'}<pre class="c-plugins__code">{JSON.stringify(
              manifest,
              null,
              2,
            )}</pre>
        {:else if dialog === 'output'}<pre class="c-plugins__code">{output ||
              '(No output)'}</pre>
        {:else if dialog === 'recovery'}<p>
            Disable all user-installed plugins. Keep packages and settings. Built-in
            worktree access remains available. You can also start WTM with --safe-mode.
          </p>
        {:else if dialog === 'rollback'}<p>
            Restore {current?.installed?.previous?.manifest.version}. Its content must still
            match the previously reviewed digest.
          </p>
        {:else}<p>
            Remove {current?.name} and its contributions. Keep package snapshots and saved data.
            Linked source folders and remote sessions are not deleted.
          </p>{/if}
      </div>
    {/snippet}
    {#snippet footer()}
      <Button disabled={busy} onclick={() => void close()}
        >{dialog === 'review' && reviewDraft ? 'Keep draft' : 'Cancel'}</Button
      >
      {#if dialog === 'review' && reviewDraft}<Button
          disabled={busy}
          onclick={() => {
            const id = reviewDraft?.id;
            void close().then(() => {
              if (id) authoring.resume(id);
            });
          }}>Refine with agent…</Button
        >{/if}
      {#if dialog === 'add'}<Button
          variant="accent"
          disabled={busy || !source.trim()}
          onclick={() => void prepare()}>{busy ? 'Reading…' : 'Review plugin'}</Button
        >
      {:else if dialog === 'review'}<Button
          variant="accent"
          disabled={busy}
          onclick={() => void install()}>Install &amp; enable</Button
        >
      {:else if dialog === 'recovery' || dialog === 'remove' || dialog === 'rollback'}<Button
          variant="accent"
          disabled={busy}
          onclick={() =>
            void run(async () => {
              await plugins.change(
                dialog === 'recovery'
                  ? 'recover'
                  : dialog === 'remove'
                    ? 'uninstall'
                    : 'rollback',
                selected,
              );
              dialog = null;
            })}
          >{dialog === 'recovery'
            ? 'Disable user plugins'
            : dialog === 'remove'
              ? 'Uninstall'
              : 'Roll back'}</Button
        >
      {:else}<Button variant="accent" onclick={() => void close()}>Done</Button>{/if}
    {/snippet}
  </Dialog>
{/if}
{#if backend}<PluginSessions
    plugin={backend.plugin}
    backend={backend.id}
    title={backend.title}
    onclose={() => {
      backend = null;
    }}
  />{/if}
