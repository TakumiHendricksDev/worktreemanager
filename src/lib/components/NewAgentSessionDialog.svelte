<script lang="ts">
  /**
   * New → New agent session: which agent, and which experience to open it in.
   *
   * The experience is chosen per session because existing sessions keep theirs: Native CLI (the
   * vendor's terminal with WTM context and tools), WTM Chat, or a plain terminal with nothing of
   * WTM's at all. The default comes from Workbench defaults, and choosing here never changes it.
   *
   * An experience that cannot open for the chosen agent is disabled with the reason beside it, and
   * the choice falls back to one that can, so Open is never silently disabled behind a checked
   * option nobody can uncheck. The reasons are the store's, the same ones its launch path refuses
   * with, so the dialog cannot offer what a click would be refused.
   */
  import { errorMessage } from '../ipc/types';
  import { extensionContext, extensionPart } from '../state/extensions.svelte';
  import {
    EXPERIENCE_NAME,
    sessions,
    type AgentExperience,
  } from '../state/sessions.svelte';
  import ExtensionPoint from './ExtensionPoint.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';

  const {
    projectId,
    worktreeId,
    onclose,
  }: { projectId: string; worktreeId: string; onclose: () => void } = $props();
  const available = $derived(sessions.options.filter((p) => p.available && p.offered));
  let provider = $state('');
  let experience = $state<AgentExperience | ''>('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  const extension = extensionContext();
  const chosen = $derived(provider || available[0]?.id || '');

  const EXPERIENCES: { id: AgentExperience; title: string; hint: string }[] = [
    {
      id: 'native',
      title: 'Native CLI with WTM integration',
      hint: 'Vendor terminal interface with WTM context, tools and available activity.',
    },
    {
      id: 'chat',
      title: EXPERIENCE_NAME.chat,
      hint: 'WTM transcript, composer and message controls.',
    },
    {
      id: 'plain',
      title: 'Plain terminal',
      hint: 'No WTM agent context, tools or activity tracking.',
    },
  ];

  /** Why each experience cannot open the chosen agent, or null where it can. */
  const problems = $derived(
    Object.fromEntries(
      EXPERIENCES.map((e) => [
        e.id,
        chosen ? sessions.experienceProblem(chosen, e.id) : null,
      ]),
    ) as Record<AgentExperience, string | null>,
  );

  /**
   * What is checked: the user's pick while it can open this agent, otherwise the default, and
   * otherwise whatever can. Switching to an agent the pick does not fit moves the check rather
   * than leaving a disabled radio selected.
   */
  const selected = $derived.by((): AgentExperience | null => {
    if (experience && problems[experience] === null) return experience;
    const fallback = chosen ? sessions.defaultExperience(chosen) : null;
    if (fallback && problems[fallback] === null) return fallback;
    return EXPERIENCES.find((e) => problems[e.id] === null)?.id ?? null;
  });

  async function open() {
    if (!chosen || !selected) return;
    busy = true;
    error = null;
    try {
      const id = await sessions.openAgent(projectId, worktreeId, chosen, {
        experience: selected,
      });
      if (!id) throw new Error(sessions.error ?? 'There is no room for another session.');
      onclose();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title="New agent session" {onclose} closeDisabled={busy}>
  {#snippet body()}
    <div class="o-stack" {@attach extensionPart('session.new', extension)}>
      <ExtensionPoint target="session.new" position="before" />
      {#if available.length === 0}
        <!-- Nothing to choose between, which is a state to explain rather than an empty select. -->
        <p role="status">
          No agent CLI is both installed and offered by this repository. Settings → Advanced
          shows the PATH WTM resolved, and the repository's <code>wtm.toml</code> says which agents
          it offers.
        </p>
      {:else}
        <Field id="native-provider" label="Agent"
          ><select
            id="native-provider"
            class="c-input"
            value={chosen}
            onchange={(event) => (provider = event.currentTarget.value)}
            >{#each available as option}<option value={option.id}>{option.label}</option
              >{/each}</select
          ></Field
        >
        <!-- A labelled radiogroup rather than a fieldset, which nothing here resets. -->
        <div class="c-field" role="radiogroup" aria-labelledby="new-session-experience">
          <span class="c-field__label" id="new-session-experience">Experience</span>
          {#each EXPERIENCES as option (option.id)}
            <Choice
              type="radio"
              name="experience"
              checked={selected === option.id}
              disabled={problems[option.id] !== null}
              onchange={() => (experience = option.id)}
            >
              {option.title}
              <span class="c-choice__hint">— {problems[option.id] ?? option.hint}</span>
            </Choice>
          {/each}
        </div>
        <p class="c-note">
          Uses your existing provider sign-in and settings. Available integration is checked
          before launch. Existing conversations keep their interface.
        </p>
      {/if}
      {#if error}<p class="c-status--danger" role="alert">{error}</p>{/if}
      <ExtensionPoint target="session.new" position="after" />
    </div>
  {/snippet}
  {#snippet footer()}<Button disabled={busy} onclick={onclose}>Cancel</Button><Button
      variant="accent"
      disabled={busy || !chosen || !selected}
      onclick={() => void open()}>{busy ? 'Opening…' : 'Open session'}</Button
    >{/snippet}
</Dialog>
