<script lang="ts">
  import { setContext as setExtensionContext } from 'svelte';
  import { EXTENSION_PROTECTED } from '../state/extensions.svelte';
  setExtensionContext(EXTENSION_PROTECTED, true);
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { open } from '@tauri-apps/plugin-dialog';
  import { commands } from '../ipc/commands';
  import { errorMessage, type AgentOption } from '../ipc/types';
  import type { PluginDraftView, PluginReview } from '../plugins';
  import { plugins } from '../state/plugins.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import { view } from '../state/view.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  import TextInput from './ui/TextInput.svelte';
  import Banner from './ui/Banner.svelte';

  const {
    showAll = false,
    onsource,
    onreview,
  }: {
    showAll?: boolean;
    onsource: (kind: 'local' | 'git') => void;
    onreview: (review: PluginReview, draft: PluginDraftView) => void;
  } = $props();
  let jobs = $state<PluginDraftView[]>([]);
  let agents = $state<AgentOption[]>([]);
  let dialog = $state<'create' | 'progress' | null>(null);
  let selected = $state<string | null>(null);
  let brief = $state('');
  let provider = $state('claude');
  let folder = $state('~/Plugins/my-plugin');
  let requestId = crypto.randomUUID();
  let busy = $state(false);
  let problem = $state<string | null>(null);
  let gone = false;
  let refreshVersion = 0;
  const current = $derived(jobs.find((j) => j.id === selected));
  const visible = $derived(showAll ? jobs : jobs.filter((j) => !j.installed));
  const available = $derived(agents.filter((a) => ['claude', 'codex'].includes(a.id)));
  const selectedAgent = $derived(available.find((a) => a.id === provider));
  const label = (job: PluginDraftView) =>
    ({
      starting: 'Starting agent',
      building: 'Building plugin',
      waiting: 'Agent needs your input',
      error: 'Plugin needs attention',
      idle: 'Agent finished · Review files',
      stopped: 'Draft saved · Agent not running',
    })[job.status];

  export async function refresh() {
    const version = ++refreshVersion;
    try {
      const result = await commands.pluginDrafts();
      if (!gone && version === refreshVersion) jobs = result;
    } catch (e) {
      if (!gone) problem = errorMessage(e);
    }
  }
  export function resume(id: string) {
    selected = id;
    problem = null;
    dialog = 'progress';
    void refresh();
  }
  export function add() {
    requestId = crypto.randomUUID();
    selected = null;
    problem = null;
    dialog = 'create';
    void commands
      .listAgents(null)
      .then((result) => {
        if (gone) return;
        agents = result;
        if (!result.find((a) => a.id === provider)?.available)
          provider =
            result.find((a) => a.available && ['claude', 'codex'].includes(a.id))?.id ??
            'claude';
      })
      .catch((e: unknown) => {
        if (!gone) problem = errorMessage(e);
      });
  }
  onMount(() => {
    void refresh();
    const events = ['plugins:drafts', 'plugins:changed', 'agent:exit'];
    const off = events.map((event) => listen(event, () => void refresh()));
    off.push(
      listen<{ event: { kind: string } }>('agent:event', (e) => {
        if (
          [
            'turn_started',
            'turn_finished',
            'failed',
            'approval_requested',
            'approval_resolved',
          ].includes(e.payload.event.kind)
        )
          void refresh();
      }),
    );
    return () => {
      gone = true;
      for (const unsubscribe of off) void unsubscribe.then((f) => f());
    };
  });
  async function perform(action: () => Promise<void>) {
    if (busy) return;
    busy = true;
    problem = null;
    try {
      await action();
    } catch (e) {
      if (!gone) problem = errorMessage(e);
    } finally {
      if (!gone) {
        busy = false;
        await refresh();
      }
    }
  }
  async function chooseFolder() {
    const result = await open({
      directory: true,
      multiple: false,
      title: 'Choose a new or empty plugin folder',
    });
    if (!gone && typeof result === 'string') folder = result;
  }
  async function start() {
    await perform(async () => {
      const job = await commands.pluginCreate(
        requestId,
        provider,
        brief.trim(),
        folder.trim(),
      );
      if (gone) return;
      selected = job.id;
      dialog = 'progress';
      await workspace.refreshProjects();
    });
  }
  async function openChat(fix = false) {
    const id = selected;
    if (!id) return;
    await perform(async () => {
      const job = await commands.pluginOpenDraft(id, fix);
      if (gone) return;
      await workspace.refreshProjects();
      await workspace.selectProject(job.folder);
      await sessions.showPluginDraft(job);
      view.show('worktree');
      dialog = null;
      settingsView.close();
    });
  }
  async function review() {
    const job = current;
    if (!job) return;
    await perform(async () => {
      const prepared = await commands.pluginReviewDraft(job.id);
      if (gone) {
        await commands.pluginCancel(prepared.token);
        return;
      }
      dialog = null;
      onreview(prepared, job);
    });
  }
</script>

{#if problem && !dialog}<Banner>{problem}</Banner>{/if}
{#if visible.length}
  <div class="c-plugins__drafts" aria-label="Plugin development tasks">
    {#each visible as job (job.id)}
      <div class="c-plugins__draft">
        <div>
          <strong>{label(job)}</strong><span class="c-plugins__muted"
            >{job.provider === 'claude' ? 'Claude' : 'Codex'} · {job.folder} · {job.installed
              ? 'Installed snapshot'
              : 'Not installed'}</span
          >
        </div>
        <Button
          size="sm"
          onclick={() => {
            selected = job.id;
            problem = null;
            dialog = 'progress';
          }}>View creation…</Button
        >
      </div>
    {/each}
  </div>
{/if}

{#if dialog}
  <Dialog
    wide
    title={dialog === 'create'
      ? 'Add plugin'
      : current?.problem || current?.validationError
        ? 'Plugin needs attention'
        : current
          ? label(current)
          : 'Plugin development'}
    onclose={() => {
      dialog = null;
    }}
    closeDisabled={busy}
  >
    {#snippet body()}
      <div class="o-stack">
        {#if problem}<Banner>{problem}</Banner>{/if}
        {#if dialog === 'create'}
          <div class="c-plugins__tabs" aria-label="How to add a plugin">
            <Button size="sm" ariaPressed={true}>Create with an agent</Button>
            <Button
              size="sm"
              variant="quiet"
              disabled={busy}
              onclick={() => {
                dialog = null;
                onsource('local');
              }}>Local folder</Button
            >
            <Button
              size="sm"
              variant="quiet"
              disabled={busy}
              onclick={() => {
                dialog = null;
                onsource('git');
              }}>Git repository</Button
            >
          </div>
          <p class="c-plugins__muted">
            Describe what you want to add or change. Build it with Claude or Codex, then
            review it before installing.
          </p>
          <Field
            id="plugin-brief"
            label="What should your plugin do?"
            help="WTM gives the agent its plugin guide and examples. You can keep refining the result in chat."
          >
            <textarea
              id="plugin-brief"
              class="c-input c-plugins__prompt"
              rows="4"
              bind:value={brief}
              disabled={busy}
              placeholder="Describe what you want WTM to do or change…"></textarea>
          </Field>
          <Field
            id="plugin-agent"
            label="Build with"
            help="Uses your existing agent sign-in and global agent settings."
          >
            <select id="plugin-agent" class="c-input" bind:value={provider} disabled={busy}>
              {#each available as agent}<option value={agent.id} disabled={!agent.available}
                  >{agent.label}{agent.available ? '' : ' · Not installed'}</option
                >{/each}
            </select>
          </Field>
          {#if selectedAgent && !selectedAgent.available}<Banner variant="warn"
              >{selectedAgent.detail}</Banner
            >{/if}
          {#if !plugins.enabled('wtm.chat')}<Banner variant="warn"
              >Enable WTM Chat in Plugins to create with an agent. Your default agent
              experience can remain Native CLI.</Banner
            >{/if}
          <Field
            id="plugin-create-folder"
            label="Plugin folder"
            help="Creates a separate Git workspace in a new or empty folder and adds it to WTM. Your current project is unchanged."
          >
            <div class="c-plugins__folder">
              <TextInput
                id="plugin-create-folder"
                bind:value={folder}
                mono
                disabled={busy}
              /><Button disabled={busy} onclick={() => void perform(chooseFolder)}
                >Choose…</Button
              >
            </div>
          </Field>
          <p class="c-plugins__note">
            The agent writes code in this folder. The plugin stays uninstalled until you
            review and enable it.
          </p>
        {:else if current}
          <div class="c-plugins__row">
            <strong
              >{current.provider === 'claude' ? 'Claude' : 'Codex'} · Plugin development</strong
            ><span class="c-plugins__muted"
              >{current.installed ? 'Installed snapshot' : 'Not installed'}</span
            >
          </div>
          {#if current.validationError || current.problem}<Banner variant="warn"
              >{current.validationError ?? current.problem}</Banner
            >{/if}
          <dl class="c-plugins__facts">
            <dt>Request</dt>
            <dd>{current.brief}</dd>
            <dt>Folder</dt>
            <dd>{current.folder}</dd>
            <dt>Status</dt>
            <dd>{label(current)}</dd>
          </dl>
          <p class="c-plugins__muted">
            Open the agent chat to answer questions, review permission requests, stop work
            or refine the plugin.
          </p>
          <p class="c-plugins__note">
            Closing this dialog keeps the task and conversation available in Develop a
            plugin. Review checks the package's files; an agent finishing does not mean they
            are valid.
          </p>
        {/if}
      </div>
    {/snippet}
    {#snippet footer()}
      <Button
        disabled={busy}
        onclick={() => {
          dialog = null;
        }}>{dialog === 'create' ? 'Cancel' : 'Close'}</Button
      >
      {#if dialog === 'create'}
        <Button
          variant="accent"
          disabled={busy ||
            !brief.trim() ||
            !folder.trim() ||
            !selectedAgent?.available ||
            !plugins.enabled('wtm.chat')}
          onclick={() => void start()}>{busy ? 'Starting…' : 'Start building'}</Button
        >
      {:else if current}
        <Button disabled={busy} onclick={() => void openChat(!!current?.validationError)}
          >{current.validationError
            ? 'Ask agent to fix'
            : current.status === 'stopped'
              ? 'Reopen agent chat'
              : 'Open agent chat'}</Button
        >
        <Button
          variant="accent"
          disabled={busy || ['building', 'starting', 'waiting'].includes(current.status)}
          onclick={() => void review()}>{busy ? 'Reading…' : 'Review plugin'}</Button
        >
      {/if}
    {/snippet}
  </Dialog>
{/if}
