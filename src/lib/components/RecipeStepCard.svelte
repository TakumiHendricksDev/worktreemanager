<script lang="ts">
  /**
   * One step of a recipe in the editor: a summary that says who runs it and what they are asked,
   * and, when open, the form that changes it.
   *
   * Changes go up through `onchange` as patches rather than by writing to `step`, which is the
   * parent's draft. The parent owns Save and Discard, so it has to own the only copy that changes.
   *
   * # Why the model, effort and mode pickers allow "Default"
   *
   * A step with nothing picked opens the way a hand-opened pane does: the repository's
   * `[agent.<id>]` first, then the provider's own default. Forcing a choice would pin every recipe
   * to today's model names, and a recipe written for Opus 5.5 should not break the day the CLI
   * offers Opus 6.
   */
  import { commandsFor } from '../agent-commands';
  import type { AgentOption, AgentSkill, RecipeStep } from '../ipc/types';
  import { INSERTABLE, agentMark, canContinue } from '../recipes';
  import { sessions } from '../state/sessions.svelte';
  import { matchSkills } from '../suggest';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Field from './ui/Field.svelte';
  import Icon from './ui/Icon.svelte';
  import TextInput from './ui/TextInput.svelte';

  const {
    step,
    index,
    steps,
    open,
    agents,
    skills,
    ontoggle,
    onchange,
    onmove,
    onduplicate,
    onremove,
  }: {
    step: RecipeStep;
    index: number;
    steps: readonly RecipeStep[];
    open: boolean;
    agents: readonly AgentOption[];
    /** What this step's agent finds on disk, or null while it is being read. */
    skills: AgentSkill[] | null;
    ontoggle: () => void;
    onchange: (patch: Partial<RecipeStep>) => void;
    onmove: (by: -1 | 1) => void;
    onduplicate: () => void;
    onremove: () => void;
  } = $props();

  const id = $derived(`recipe-step-${index}`);
  const agentLabel = (agent: string) => agents.find((a) => a.id === agent)?.label ?? agent;
  const capability = $derived(sessions.capabilities[step.agent] ?? null);

  $effect(() => {
    void sessions.loadCapability(step.agent);
  });

  const model = $derived(
    capability?.models.find((m) => m.id === step.model) ??
      capability?.models.find((m) => m.isDefault) ??
      null,
  );
  const efforts = $derived(model?.efforts ?? []);
  const modelLabel = $derived(
    step.model === null
      ? null
      : (capability?.models.find((m) => m.id === step.model)?.label ?? step.model),
  );
  const modeLabel = $derived(
    step.mode === null
      ? null
      : (capability?.modes.find((m) => m.id === step.mode)?.label ?? step.mode),
  );
  const unsandboxed = $derived(
    capability?.modes.find((m) => m.id === step.mode)?.risk === 'unsandboxed',
  );

  /** Agent · model · effort · mode, leaving out what is left to the default. */
  const who = $derived(
    [agentLabel(step.agent), modelLabel, step.effort, modeLabel]
      .filter((part): part is string => part !== null && part !== '')
      .join(' · '),
  );

  const previousNumber = $derived(index);
  const continuable = $derived(canContinue(steps, index));

  let query = $state('');
  const listed = $derived(commandsFor(step.agent, skills ?? []));
  const shown = $derived(
    query.trim() === ''
      ? listed.map((s) => ({ value: s.name, detail: s.description ?? '', scope: s.scope }))
      : matchSkills(listed, query.trim()).map((s) => ({
          value: s.value,
          detail: s.detail,
          scope: listed.find((l) => l.name === s.value)?.scope ?? null,
        })),
  );
  const typed = $derived(query.trim().replace(/^\//, ''));

  function chooseAgent(agent: string) {
    // A different provider's model, effort and mode mean nothing to this one, and continuing a
    // conversation held by another CLI is impossible, so all four go back to their defaults.
    const patch: Partial<RecipeStep> = { agent, model: null, effort: null, mode: null };
    if (steps[index - 1]?.agent !== agent) patch.conversation = 'new';
    onchange(patch);
  }

  function insert(variable: string) {
    const token = `{{${variable}}}`;
    if (step.kind === 'prompt') {
      const sep = step.prompt === '' || step.prompt.endsWith(' ') ? '' : ' ';
      onchange({ prompt: `${step.prompt}${sep}${token}` });
    } else {
      onchange({ args: step.args === '' ? token : `${step.args} ${token}` });
    }
  }

  /** The `{{…}}` in a summary line, set apart from the words around it. */
  function parts(text: string): { text: string; variable: boolean }[] {
    return text
      .split(/(\{\{[^}]*\}\})/)
      .filter((p) => p !== '')
      .map((p) => ({ text: p, variable: p.startsWith('{{') }));
  }
</script>

<li class="c-recipe-step" class:is-open={open}>
  <div class="c-recipe-step__rail"><span class="c-recipe-step__num">{index + 1}</span></div>
  <div class="c-recipe-step__card">
    <button
      type="button"
      class="c-recipe-step__summary"
      aria-expanded={open}
      aria-controls="{id}-edit"
      onclick={ontoggle}
    >
      <span class="c-recipe-step__title">{step.name || `Step ${index + 1}`}</span>
      <span class="c-recipe-step__who">
        <span class="c-agent-mark c-agent-mark--{step.agent}" aria-hidden="true"
          >{agentMark(step.agent)}</span
        >
        {who}
      </span>
      <span class="c-recipe-step__what">
        <span class="c-recipe-step__kind">{step.kind === 'skill' ? 'Skill' : 'Prompt'}</span
        >
        <span class="c-recipe-step__text">
          {#if step.kind === 'skill'}
            <code>/{step.skill.replace(/^\//, '') || '…'}</code>
            {#each parts(step.args) as part, i (i)}{#if part.variable}<code
                  >{part.text}</code
                >{:else}{part.text}{/if}{/each}
          {:else if step.prompt.trim() === ''}
            …
          {:else}
            {#each parts(step.prompt) as part, i (i)}{#if part.variable}<code
                  >{part.text}</code
                >{:else}{part.text}{/if}{/each}
          {/if}
        </span>
      </span>
      <span class="c-recipe-step__session">
        {#if index > 0 && step.conversation === 'continue'}
          <span
            ><Icon name="comment" size={12} /> Continues step {previousNumber}’s
            conversation</span
          >
        {:else}
          <span><Icon name="plus" size={12} /> New session</span>
        {/if}
        {#if index > 0 && step.previousReply && step.conversation === 'new'}
          <span><Icon name="link" size={12} /> Gets step {previousNumber}’s reply</span>
        {/if}
      </span>
    </button>

    {#if open}
      <div class="c-recipe-step__edit" id="{id}-edit">
        <div class="c-recipe-step__grid c-recipe-step__grid--2">
          <Field id="{id}-name" label="Step name">
            <TextInput
              id="{id}-name"
              value={step.name}
              placeholder="Plan, Review, Fix…"
              oninput={(e) =>
                onchange({ name: (e.currentTarget as HTMLInputElement).value })}
            />
          </Field>
          <Field id="{id}-failure" label="If it fails">
            <select
              id="{id}-failure"
              class="c-select"
              value={step.onFailure}
              onchange={(e) =>
                onchange({
                  onFailure: (e.currentTarget as HTMLSelectElement).value as
                    'stop' | 'continue',
                })}
            >
              <option value="stop">Stop the recipe</option>
              <option value="continue">Carry on to the next step</option>
            </select>
          </Field>
        </div>

        <div class="c-recipe-step__grid">
          <Field id="{id}-agent" label="Agent">
            <select
              id="{id}-agent"
              class="c-select"
              value={step.agent}
              onchange={(e) => chooseAgent((e.currentTarget as HTMLSelectElement).value)}
            >
              {#each agents as agent (agent.id)}
                <option value={agent.id}
                  >{agent.label}{agent.available ? '' : '  (not installed)'}</option
                >
              {/each}
            </select>
          </Field>
          <Field id="{id}-model" label="Model">
            <select
              id="{id}-model"
              class="c-select"
              value={step.model ?? ''}
              disabled={!capability}
              onchange={(e) => {
                const value = (e.currentTarget as HTMLSelectElement).value;
                onchange({ model: value === '' ? null : value, effort: null });
              }}
            >
              <option value="">Default</option>
              {#each capability?.models ?? [] as option (option.id)}
                <option value={option.id}>{option.label}</option>
              {/each}
              {#if step.model !== null && !capability?.models.some((m) => m.id === step.model)}
                <option value={step.model}>{step.model}</option>
              {/if}
            </select>
          </Field>
          <Field id="{id}-effort" label="Effort">
            <select
              id="{id}-effort"
              class="c-select"
              value={step.effort ?? ''}
              disabled={efforts.length === 0}
              onchange={(e) => {
                const value = (e.currentTarget as HTMLSelectElement).value;
                onchange({ effort: value === '' ? null : value });
              }}
            >
              <option value="">Default</option>
              {#each efforts as option (option.effort)}
                <option value={option.effort}>{option.effort}</option>
              {/each}
            </select>
          </Field>
          <Field id="{id}-mode" label="Mode">
            <select
              id="{id}-mode"
              class="c-select"
              value={step.mode ?? ''}
              disabled={!capability}
              onchange={(e) => {
                const value = (e.currentTarget as HTMLSelectElement).value;
                onchange({ mode: value === '' ? null : value });
              }}
            >
              <option value="">Default</option>
              {#each capability?.modes ?? [] as option (option.id)}
                <option value={option.id}>{option.label}</option>
              {/each}
            </select>
          </Field>
        </div>
        {#if unsandboxed}
          <p class="c-field__help c-status--warn">
            This mode runs with no approvals at all, unattended, for as long as the step
            takes.
          </p>
        {/if}

        <div class="c-field">
          <span class="c-field__label" id="{id}-kind">It runs</span>
          <div class="c-seg" role="radiogroup" aria-labelledby="{id}-kind">
            <button
              type="button"
              role="radio"
              aria-checked={step.kind === 'skill'}
              class="c-seg__option"
              class:is-selected={step.kind === 'skill'}
              onclick={() => onchange({ kind: 'skill' })}>A skill</button
            >
            <button
              type="button"
              role="radio"
              aria-checked={step.kind === 'prompt'}
              class="c-seg__option"
              class:is-selected={step.kind === 'prompt'}
              onclick={() => onchange({ kind: 'prompt' })}>A prompt</button
            >
          </div>
        </div>

        {#if step.kind === 'skill'}
          <div class="c-recipe-step__grid c-recipe-step__grid--2">
            <div class="c-field">
              <label class="c-field__label" for="{id}-skill-search">Skill</label>
              <div class="c-skill">
                <div class="c-skill__search">
                  <Icon name="search" size={14} />
                  <input
                    id="{id}-skill-search"
                    class="c-skill__query"
                    placeholder="Search {agentLabel(step.agent)}’s skills"
                    bind:value={query}
                    autocomplete="off"
                    spellcheck="false"
                  />
                </div>
                <div class="c-skill__list" role="listbox" aria-label="Skills">
                  {#if skills === null}
                    <p class="c-skill__empty">Reading skills…</p>
                  {/if}
                  {#if typed !== '' && !listed.some((s) => s.name === typed)}
                    <button
                      type="button"
                      role="option"
                      aria-selected={step.skill === typed}
                      class="c-skill__opt"
                      class:is-selected={step.skill === typed}
                      onclick={() => onchange({ skill: typed })}
                    >
                      <span class="c-skill__name">/{typed}</span>
                      <span class="c-skill__desc">Use this name — a plugin’s, say</span>
                      <span class="c-skill__scope"></span>
                    </button>
                  {/if}
                  {#each shown as option (option.value)}
                    <button
                      type="button"
                      role="option"
                      aria-selected={step.skill === option.value}
                      class="c-skill__opt"
                      class:is-selected={step.skill === option.value}
                      onclick={() => onchange({ skill: option.value })}
                    >
                      <span class="c-skill__name">/{option.value}</span>
                      <span class="c-skill__desc">{option.detail}</span>
                      <span class="c-skill__scope">{option.scope ?? ''}</span>
                    </button>
                  {/each}
                </div>
              </div>
              <p class="c-field__help">
                Read from {agentLabel(step.agent)}’s skill folders and this repository’s. A
                plugin’s skills aren’t listed; type the name. A skill missing when the
                recipe runs fails this step.
              </p>
            </div>
            <Field
              id="{id}-args"
              label="Arguments"
              help="Sent as /{step.skill.replace(/^\//, '') ||
                'skill'} {step.args}, as the composer sends it."
            >
              <TextInput
                id="{id}-args"
                mono
                value={step.args}
                placeholder="optional"
                oninput={(e) =>
                  onchange({ args: (e.currentTarget as HTMLInputElement).value })}
              />
            </Field>
          </div>
        {:else}
          <Field id="{id}-prompt" label="Prompt">
            <textarea
              id="{id}-prompt"
              class="c-textarea"
              rows="4"
              value={step.prompt}
              oninput={(e) =>
                onchange({ prompt: (e.currentTarget as HTMLTextAreaElement).value })}
            ></textarea>
          </Field>
        {/if}

        <div class="c-vars">
          <span>Insert</span>
          {#each INSERTABLE as variable (variable)}
            <button
              type="button"
              class="c-var"
              title="Filled in when the recipe starts"
              onclick={() => insert(variable)}>{`{{${variable}}}`}</button
            >
          {/each}
          <span
            >· a variable the action doesn’t have is left empty and named in the run.</span
          >
        </div>

        {#if index > 0}
          <div class="c-field">
            <span class="c-field__label">Conversation</span>
            <div class="c-recipe-step__radios">
              <Choice
                type="radio"
                name="{id}-conversation"
                checked={step.conversation === 'new'}
                onchange={() => onchange({ conversation: 'new' })}
              >
                Start a new {agentLabel(step.agent)} session
              </Choice>
              <Choice
                type="radio"
                name="{id}-conversation"
                checked={step.conversation === 'continue'}
                disabled={!continuable}
                onchange={() =>
                  onchange({ conversation: 'continue', previousReply: false })}
              >
                Continue step {previousNumber}’s conversation
                {#if !continuable}
                  <span class="c-choice__hint"
                    >— step {previousNumber} runs on {agentLabel(
                      steps[index - 1]?.agent ?? '',
                    )}</span
                  >
                {/if}
              </Choice>
              <Choice
                checked={step.previousReply && step.conversation === 'new'}
                disabled={step.conversation === 'continue'}
                onchange={(checked) => onchange({ previousReply: checked })}
              >
                Give it step {previousNumber}’s final reply
                <span class="c-choice__hint"
                  >{step.conversation === 'continue'
                    ? '— it already has it'
                    : '— appended under the prompt'}</span
                >
              </Choice>
            </div>
          </div>
        {/if}

        <div class="c-recipe-step__tools">
          <Button
            variant="quiet"
            size="sm"
            disabled={index === 0}
            onclick={() => onmove(-1)}
          >
            <Icon name="chevron-up" size={12} /> Earlier
          </Button>
          <Button
            variant="quiet"
            size="sm"
            disabled={index === steps.length - 1}
            onclick={() => onmove(1)}
          >
            <Icon name="chevron-down" size={12} /> Later
          </Button>
          <Button variant="quiet" size="sm" onclick={onduplicate}>
            <Icon name="copy" size={12} /> Duplicate
          </Button>
          <span class="c-recipe-step__spacer"></span>
          <Button
            variant="quiet"
            size="sm"
            disabled={steps.length === 1}
            onclick={onremove}
          >
            <Icon name="trash" size={12} /> Remove step
          </Button>
        </div>
      </div>
    {/if}
  </div>
</li>
