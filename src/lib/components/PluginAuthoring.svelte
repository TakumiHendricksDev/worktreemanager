<script lang="ts">
  /**
   * Add plugin → Create with an agent, and the drafts it leaves behind.
   *
   * A draft is a visible WTM Chat conversation in a separate repository folder of its own, so the
   * agent never writes into the one you were working in, and the conversation survives closing
   * Settings or a restart. Finishing a turn never activates anything: Review plugin runs the
   * ordinary package validation and review, and only Install & enable there turns the result on.
   *
   * Drafts are re-read when Rust says they changed (`plugins:drafts`) and when one of their own
   * conversations starts or finishes a turn or asks for approval. Every session's events used to
   * trigger a read, so any busy chat elsewhere in the window kept re-listing drafts.
   *
   * Removing a draft forgets its record only. Its folder, its files and its place in the sidebar
   * stay, and Rust refuses while its conversation is still open, because the record is the only
   * thing that ties that conversation's events back to the draft.
   */
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { open } from '@tauri-apps/plugin-dialog';
  import { commands } from '../ipc/commands';
  import {
    errorMessage,
    type AgentEventEnvelope,
    type AgentExit,
    type AgentOption,
  } from '../ipc/types';
  import type { PluginDraftView, PluginReview, PluginSourceKind } from '../plugins';
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
    onsource: (kind: PluginSourceKind) => void;
    onreview: (review: PluginReview, draft: PluginDraftView) => void;
  } = $props();
  let jobs = $state<PluginDraftView[]>([]);
  let agents = $state<AgentOption[]>([]);
  let dialog = $state<'create' | 'progress' | 'remove' | null>(null);
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
  /** The draft's own banner already says this; the same text twice is one problem, not two. */
  const extra = $derived(
    problem && problem !== (current?.validationError ?? current?.problem) ? problem : null,
  );
  const label = (job: PluginDraftView) =>
    ({
      starting: 'Starting agent',
      building: 'Building plugin',
      waiting: 'Agent needs your input',
      error: 'Plugin needs attention',
      idle: 'Agent finished · Review files',
      stopped: 'Draft saved · Agent not running',
    })[job.status];
  const providerName = (job: PluginDraftView) =>
    job.provider === 'claude' ? 'Claude' : 'Codex';

  function title(): string {
    if (dialog === 'create') return 'Add plugin';
    if (dialog === 'remove') return 'Remove draft?';
    if (current?.problem || current?.validationError) return 'Plugin needs attention';
    return current ? label(current) : 'Plugin development';
  }

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
  /** Whether a session is one of the drafts' own conversations. */
  function drafting(session: string): boolean {
    return jobs.some((j) => j.session === session);
  }
  onMount(() => {
    void refresh();
    const off = [
      listen('plugins:drafts', () => void refresh()),
      listen('plugins:changed', () => void refresh()),
      listen<AgentExit>('agent:exit', (e) => {
        if (drafting(e.payload.session)) void refresh();
      }),
      listen<AgentEventEnvelope>('agent:event', (e) => {
        if (
          drafting(e.payload.session) &&
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
    ];
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
    if (
      busy ||
      !brief.trim() ||
      !folder.trim() ||
      !selectedAgent?.available ||
      !plugins.enabled('wtm.chat')
    )
      return;
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
  async function remove() {
    const id = selected;
    if (!id) return;
    await perform(async () => {
      await commands.pluginDeleteDraft(id);
      dialog = null;
      selected = null;
    });
  }
</script>

{#if problem && !dialog}<Banner>{problem}</Banner>{/if}
{#if visible.length}
  <ul class="o-plain-list c-plugins__drafts" aria-label="Plugin drafts">
    {#each visible as job (job.id)}
      <li class="o-row o-row--between c-plugins__draft">
        <div class="o-stack o-stack--tight">
          <strong>{label(job)}</strong><span class="c-note"
            >{providerName(job)} · {job.folder} · {job.installed
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
      </li>
    {/each}
  </ul>
{/if}

{#if dialog}
  <Dialog
    wide
    title={title()}
    onclose={() => {
      dialog = null;
    }}
    closeDisabled={busy}
    onsubmit={(event) => {
      event.preventDefault();
      if (dialog === 'create') void start();
    }}
  >
    {#snippet body()}
      <div class="o-stack">
        {#if dialog === 'create'}
          <div class="o-row" role="group" aria-label="How to add a plugin">
            <Button size="sm" ariaPressed={true}>Create with an agent</Button>
            <Button
              size="sm"
              variant="quiet"
              ariaPressed={false}
              disabled={busy}
              onclick={() => {
                dialog = null;
                onsource('local');
              }}>Local folder</Button
            >
            <Button
              size="sm"
              variant="quiet"
              ariaPressed={false}
              disabled={busy}
              onclick={() => {
                dialog = null;
                onsource('git');
              }}>Git repository</Button
            >
          </div>
          <p class="c-note">
            Describe what you want to add or change. Build it with Claude or Codex, then
            review it before installing.
          </p>
          <Field
            id="plugin-brief"
            label="What should your plugin do?"
            help="wtm gives the agent its plugin guide and examples. You can keep refining the result in chat."
          >
            <textarea
              id="plugin-brief"
              class="c-input c-plugins__prompt"
              rows="4"
              bind:value={brief}
              disabled={busy}
              placeholder="Describe what you want wtm to do or change…"></textarea>
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
              experience can stay Native CLI.</Banner
            >{/if}
          <Field
            id="plugin-create-folder"
            label="Plugin folder"
            help="A new or empty folder; ~ is your home folder. wtm makes it a separate Git repository and adds it to the sidebar. Your current project is unchanged."
          >
            <div class="o-row c-plugins__inline">
              <TextInput
                id="plugin-create-folder"
                bind:value={folder}
                mono
                disabled={busy}
                placeholder="~/Plugins/my-plugin"
              /><Button disabled={busy} onclick={() => void perform(chooseFolder)}
                >Choose folder…</Button
              >
            </div>
          </Field>
          <p class="c-note">
            The agent writes code in this folder. The plugin stays uninstalled until you
            review and enable it.
          </p>
        {:else if dialog === 'remove' && current}
          <p>
            wtm stops listing this draft under Develop a plugin and forgets its link to the
            {providerName(current)} conversation.
          </p>
          <p>
            The folder {current.folder} and everything in it are kept, and so is its place in
            the sidebar. {current.installed
              ? 'The installed plugin built from it stays installed.'
              : ''}
          </p>
        {:else if current}
          <div class="o-row o-row--between">
            <strong>{providerName(current)} · Plugin development</strong><span
              class="c-note"
              >{current.installed ? 'Installed snapshot' : 'Not installed'}</span
            >
          </div>
          {#if current.validationError || current.problem}<Banner variant="warn"
              >{current.validationError ?? current.problem}</Banner
            >{/if}
          <dl class="o-facts">
            <dt>Request</dt>
            <dd>{current.brief}</dd>
            <dt>Folder</dt>
            <dd>{current.folder}</dd>
            <dt>Status</dt>
            <dd>{label(current)}</dd>
          </dl>
          <p class="c-note">
            Open the agent chat to answer questions, review permission requests, stop work
            or refine the plugin.
          </p>
          <p class="c-note">
            Closing this dialog keeps the draft and its conversation in Develop a plugin.
            Review checks the plugin’s files; an agent finishing does not mean they are
            valid.
          </p>
        {/if}
        {#if extra}<Banner>{extra}</Banner>{/if}
      </div>
    {/snippet}
    {#snippet footer()}
      {#if dialog === 'remove'}
        <Button
          disabled={busy}
          onclick={() => {
            problem = null;
            dialog = 'progress';
          }}>Cancel</Button
        >
        <Button variant="danger-solid" disabled={busy} onclick={() => void remove()}
          >{busy ? 'Removing…' : 'Remove draft'}</Button
        >
      {:else}
        <Button
          disabled={busy}
          onclick={() => {
            dialog = null;
          }}>{dialog === 'create' ? 'Cancel' : 'Close'}</Button
        >
        {#if dialog === 'create'}
          <Button
            type="submit"
            variant="accent"
            disabled={busy ||
              !brief.trim() ||
              !folder.trim() ||
              !selectedAgent?.available ||
              !plugins.enabled('wtm.chat')}>{busy ? 'Starting…' : 'Start building'}</Button
          >
        {:else if current}
          <Button
            variant="quiet"
            disabled={busy}
            onclick={() => {
              problem = null;
              dialog = 'remove';
            }}>Remove draft…</Button
          >
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
      {/if}
    {/snippet}
  </Dialog>
{/if}
