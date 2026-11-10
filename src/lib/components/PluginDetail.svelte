<script lang="ts">
  /**
   * One plugin in the Installed tab: whether it is on, what it adds, what it may do, where it came
   * from, and the ways to update, roll back or remove it.
   *
   * The parts a package adds outside wtm's own screens are one list driven by `packageParts`, each
   * kind with its action in `ACTIONS`, so a seventh kind is one entry rather than a seventh copy of
   * the row markup.
   *
   * # The Enabled switch
   *
   * Rust refuses some changes on purpose: Home with sessions running, WTM Chat while Home needs it,
   * Native CLI while it is the default experience. The checkbox is controlled by the registry,
   * which a refusal leaves unchanged, and Svelte writes `checked` only when that value changes, so
   * a refused click left the box showing the state the user asked for. `toggling` shows the asked
   * state while the change is in flight and lets go after, which moves the box back if it failed.
   *
   * # Trust copy
   *
   * Whether a package is described as native code follows `executes`, the host's own rule, so a
   * package that only declares host access is never called isolated.
   */
  import { commands } from '../ipc/commands';
  import {
    executes,
    packageParts,
    PACKAGE_PART_LABELS,
    PLUGIN_SLOTS,
    type PackagePart,
    type PackagePartKind,
    type PluginInstalled,
    type PluginRow,
  } from '../plugins';
  import { extensions } from '../state/extensions.svelte';
  import { plugins, PluginTask } from '../state/plugins.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';
  import PluginAccess from './PluginAccess.svelte';
  import PluginContributions from './PluginContributions.svelte';
  import PluginSessions from './PluginSessions.svelte';

  const {
    row,
    onupdate,
    onrollback,
    onuninstall,
    ondefaults,
    onnotice,
  }: {
    row: PluginRow;
    onupdate: (installed: PluginInstalled) => void;
    onrollback: (row: PluginRow) => void;
    onuninstall: (row: PluginRow) => void;
    /** Show Workbench defaults, where experiences and regions are chosen. */
    ondefaults: () => void;
    onnotice: (message: string) => void;
  } = $props();

  const task = new PluginTask();
  let toggling = $state<boolean | null>(null);
  let running = $state<string | null>(null);
  let viewing = $state<{ title: string; text: string } | null>(null);
  let backend = $state<{ id: string; title: string } | null>(null);

  const manifest = $derived(row.installed?.current.manifest);
  const parts = $derived(manifest ? packageParts(manifest) : []);
  const projectId = $derived(workspace.activeProjectId);
  const worktreeId = $derived(workspace.selectedWorktreeId);
  const needsWorktree = $derived(
    !worktreeId && parts.some((p) => p.kind === 'command' || p.kind === 'panel'),
  );

  type PartAction = { label: string; disabled: boolean; run: () => void };
  const ACTIONS: Record<PackagePartKind, (part: PackagePart) => PartAction> = {
    terminal: (part) => ({
      label: 'Open terminal',
      disabled: !row.enabled || !projectId || !worktreeId,
      run: () => {
        if (!projectId || !worktreeId) return;
        void sessions.openPluginTerminal(
          projectId,
          worktreeId,
          row.id,
          part.id,
          part.title,
        );
        settingsView.close();
      },
    }),
    view: () => ({ label: 'Choose region…', disabled: !row.enabled, run: ondefaults }),
    palette: () => ({
      label: 'Choose palette…',
      disabled: !row.enabled,
      run: () => settingsView.show('appearance'),
    }),
    command: (part) => ({
      label: running === part.id ? 'Running…' : 'Run',
      disabled: task.busy || !row.enabled || !worktreeId,
      run: () => void runCommand(part),
    }),
    panel: (part) => ({
      label: 'Open',
      disabled: task.busy || !row.enabled || !worktreeId,
      run: () => void openPanel(part),
    }),
    backend: (part) => ({
      label: 'Browse sessions…',
      disabled: !row.enabled,
      run: () => {
        backend = { id: part.id, title: part.title };
      },
    }),
  };

  async function toggle(enabled: boolean) {
    const { id, name } = row;
    toggling = enabled;
    const ok = await task.run(() => plugins.setEnabled(id, enabled));
    toggling = null;
    if (ok) onnotice(`${enabled ? 'Enabled' : 'Disabled'} ${name}.`);
  }

  async function runCommand(part: PackagePart) {
    if (!projectId || !worktreeId) return;
    const project = projectId,
      worktree = worktreeId;
    running = part.id;
    let text = '';
    const ok = await task.run(async () => {
      text = await commands.pluginRun(project, worktree, row.id, part.id);
    });
    running = null;
    if (ok) viewing = { title: `${part.title} output`, text: text || '(No output)' };
  }

  async function openPanel(part: PackagePart) {
    if (!projectId || !worktreeId) return;
    const project = projectId,
      worktree = worktreeId;
    if (await task.run(() => commands.pluginPanel(project, worktree, row.id, part.id)))
      settingsView.close();
  }

  async function restart(id: string) {
    if (await task.run(() => commands.extensionRestart(id))) {
      delete extensions.problems[id];
      onnotice(`Restarted ${row.name}'s runtime.`);
    }
  }
</script>

<div class="o-stack o-stack--loose">
  <div class="o-row o-row--between o-row--top">
    <div class="o-stack o-stack--tight">
      <h3 class="c-pane-title">
        {row.name} <span class="c-badge">{row.version}</span>
      </h3>
      <p class="c-note">{row.id}</p>
    </div>
    <Choice
      checked={toggling ?? row.enabled}
      disabled={task.busy}
      onchange={(enabled) => void toggle(enabled)}>Enabled</Choice
    >
  </div>
  {#if task.error}<Banner>{task.error}</Banner>{/if}
  <p class="u-muted">{row.description}</p>
  {#if row.id === 'wtm.native-cli'}
    <p class="c-note">
      Use the original CLI experience, including its own mods. wtm manages the pane; the CLI
      manages the conversation.
    </p>
    <div class="c-plugins__section o-row o-row--between">
      <div>
        Default for new local sessions
        <p class="c-note">
          Currently {plugins.state.registry.experience === 'wtm.chat'
            ? 'WTM Chat'
            : 'Native CLI'}
        </p>
      </div>
      <Button onclick={ondefaults}>Choose experience…</Button>
    </div>
  {:else if row.id === 'wtm.home'}
    <p class="c-note">
      Disabling Home removes its entry points and stops it from loading. Close its running
      sessions first. Saved conversations and worktrees are kept.
    </p>
  {/if}
  {#if manifest || row.slots.length}
    <div class="c-plugins__section o-stack">
      <h4 class="c-section-heading">What this plugin adds</h4>
      {#if manifest}
        {#if parts.length}
          <ul class="o-plain-list o-stack">
            {#each parts as part (part.kind + part.id)}
              {@const action = ACTIONS[part.kind](part)}
              <li class="o-row o-row--between">
                <span class="o-stack o-stack--tight"
                  >{part.title}<span class="c-note"
                    >{PACKAGE_PART_LABELS[
                      part.kind
                    ]}{#if part.detail}{` · ${part.detail}`}{/if}</span
                  ></span
                ><Button size="sm" disabled={action.disabled} onclick={action.run}
                  >{action.label}</Button
                >
              </li>
            {/each}
          </ul>
        {/if}
        {#if needsWorktree}<p class="c-note">
            Select a worktree to use its commands and panels.
          </p>{/if}
        <PluginContributions plugin={manifest.id} level={5} />
      {:else}
        <p>
          Fills the {row.slots.map((s) => PLUGIN_SLOTS[s]).join(', ')} region. A plugin view can
          take its place; choose one in Workbench defaults.
        </p>
        <div><Button onclick={ondefaults}>Choose region…</Button></div>
      {/if}
    </div>
  {/if}
  <div class="c-plugins__section o-stack">
    <h4 class="c-section-heading">Access &amp; source</h4>
    {#if !manifest}
      <p class="c-note">Part of wtm. It updates with the app.</p>
    {:else if !executes(manifest)}
      <p class="c-note">
        No native code. Its panels and replacement views run in isolated pages with no
        direct network access, and its styles change only the parts of wtm the public
        contract names.
      </p>
    {/if}
    {#if manifest}
      <PluginAccess {manifest} executable={executes(manifest)} level={5} />
      {#if extensions.problems[manifest.id]}<Banner variant="error"
          >{extensions.problems[manifest.id]}</Banner
        >{/if}
      {#if manifest.extensions.runtime.length}
        {@const id = manifest.id}
        <div>
          <Button
            size="sm"
            disabled={task.busy || !plugins.enabled(id)}
            onclick={() => void restart(id)}>Restart runtime</Button
          >
        </div>
      {/if}
    {/if}
    <dl class="o-facts">
      <dt>Source</dt>
      <dd>{row.installed?.current.source ?? 'Bundled with wtm'}</dd>
      <dt>Revision</dt>
      <dd>{row.installed?.current.revision ?? 'Follows your wtm release'}</dd>
      {#if manifest}
        <dt>Compatibility</dt>
        <dd>Plugin API {manifest.api}</dd>
      {/if}
      <dt>Status</dt>
      <dd>{row.enabled ? 'Enabled' : 'Disabled'}</dd>
    </dl>
  </div>
  {#if row.installed}
    {@const installed = row.installed}
    <div class="c-plugins__section o-row c-plugins__actions">
      <Button onclick={() => onupdate(installed)}
        >{installed.current.kind === 'git' ? 'Review update…' : 'Reload & review…'}</Button
      >{#if installed.previous}<Button onclick={() => onrollback(row)}>Roll back…</Button
        >{/if}<Button
        onclick={() => {
          viewing = {
            title: `${row.name} manifest`,
            text: JSON.stringify(installed.current.manifest, null, 2),
          };
        }}>View manifest</Button
      ><Button variant="danger-outline" onclick={() => onuninstall(row)}>Uninstall…</Button>
    </div>
  {/if}
</div>

{#if viewing}
  {@const shown = viewing}
  <Dialog title={shown.title} onclose={() => (viewing = null)}>
    {#snippet body()}<pre class="c-plugins__code">{shown.text}</pre>{/snippet}
    {#snippet footer()}<Button variant="accent" onclick={() => (viewing = null)}
        >Done</Button
      >{/snippet}
  </Dialog>
{/if}
{#if backend && manifest}<PluginSessions
    plugin={manifest.id}
    backend={backend.id}
    title={backend.title}
    onclose={() => {
      backend = null;
    }}
  />{/if}
