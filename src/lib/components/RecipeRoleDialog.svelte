<script lang="ts" module>
  import type { RoleProvider } from '../ipc/types';

  /** Edit a role, or make one and put it into a recipe's stage. */
  export type RoleDialogMode =
    | { kind: 'edit'; id: string; provider: RoleProvider | null }
    | { kind: 'new'; recipe: string; stage: number };
</script>

<script lang="ts">
  /**
   * One role's editor, for Settings › Recipes.
   *
   * # Apply checks; Save writes
   *
   * Apply puts the role into the page's draft, and Save on the page writes the draft to the config.
   * Apply first asks Rust whether the draft with this role would load — the whole project, validated
   * as loading does — so a mistake in this role is shown here, under the field it is about, while
   * the fields are still in front of you. A problem the role did not cause, elsewhere in the draft,
   * does not hold the dialog open: the page shows it where it is.
   *
   * # Moving a role to another provider
   *
   * The Reviews tab's Move role to Codex opens this with the provider already changed. Nothing is
   * saved by that click: the change is a field like any other, validated by Apply and written by
   * Save. The model and effort are cleared with it, because they are spelled the old provider's way
   * and would not start on the new one.
   *
   * # Deleting says what it takes with it
   *
   * A role a recipe stage or another role's fallback names is taken out of those too, and the
   * confirmation lists them. A role the planner, the designer or the implementer is, or one your
   * defaults declare beneath this file, cannot be deleted here, and the dialog says where to change
   * it instead.
   */
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import {
    DEFAULT_TIMEOUT_MS,
    ID_PATTERN,
    INPUTS,
    MAX_FALLBACKS,
    MAX_RETRIES,
    PROVIDERS,
    minutesOf,
    addToStage,
    deleteRole,
    errorAt,
    fallbackChoices,
    fallbackRoles,
    newRole,
    onProvider,
    providerLabel,
    putRole,
    referencesTo,
    refusalKey,
    refusalOf,
    splitList,
    type ImplementSpec,
    type RecipesRefusal,
    type RoleInput,
    type RoleSpec,
    type WorkflowDraft,
    type WorkflowFile,
    type WorkflowHome,
  } from '../recipes';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  import TextInput from './ui/TextInput.svelte';
  import CodeText from './CodeText.svelte';

  const {
    projectId,
    mode,
    draft,
    file,
    issue,
    checks,
    onapply,
    onclose,
  }: {
    projectId: string;
    mode: RoleDialogMode;
    draft: WorkflowDraft;
    file: WorkflowFile;
    issue: {
      planner: string | null;
      designer: string | null;
      implement: ImplementSpec | null;
    };
    checks: { id: string; label: string }[];
    /** The draft with the role applied, and what validation said about the rest of it, if anything. */
    onapply: (next: WorkflowDraft, elsewhere: RecipesRefusal | null) => void;
    onclose: () => void;
  } = $props();

  // Read once: the dialog edits a copy, and a page refresh behind it must not reset the form.
  // svelte-ignore state_referenced_locally
  const original: RoleSpec | null =
    mode.kind === 'edit' ? (draft.role[mode.id] ?? null) : null;
  // svelte-ignore state_referenced_locally
  const start: RoleSpec =
    mode.kind === 'edit' && original
      ? onProvider(original, mode.provider ?? original.provider)
      : newRole();
  // svelte-ignore state_referenced_locally
  const moved =
    mode.kind === 'edit' && original && mode.provider && mode.provider !== original.provider
      ? { from: original.provider, to: mode.provider }
      : null;

  // svelte-ignore state_referenced_locally
  let id = $state(mode.kind === 'edit' ? mode.id : '');
  let objective = $state(start.objective);
  let instructions = $state(start.instructions ?? '');
  let provider = $state<RoleProvider>(start.provider);
  let model = $state(start.model ?? '');
  let effort = $state(start.effort ?? '');
  let skills = $state((start.skills ?? []).join(', '));
  let paths = $state((start.paths ?? []).join(', '));
  let chosenChecks = $state<string[]>([...(start.checks ?? [])]);
  let inputs = $state<RoleInput[]>([...start.inputs]);
  // svelte-ignore state_referenced_locally
  const startMinutes = start.timeout_ms === null ? '' : minutesOf(start.timeout_ms);
  // Written back only when changed: the field shows minutes, and a timeout set in milliseconds by
  // hand is not rounded to what the field can show by an Apply that did not touch it.
  let minutes = $state(startMinutes);
  let retries = $state(start.retries ?? 0);
  let falls = $state<string[]>(fallbackRoles(start));
  let askFirst = $state(fallbackRoles(start).length === 0);

  let busy = $state(false);
  let error = $state<string | null>(null);
  let fieldErrors = $state<Record<string, string>>({});
  let confirmingDelete = $state(false);

  const title = $derived(mode.kind === 'edit' ? `Edit role ${mode.id}` : 'New role');
  const choices = $derived(fallbackChoices(id, role(), draft.role));
  const references = $derived(
    mode.kind === 'edit' ? referencesTo(mode.id, draft, issue) : [],
  );
  const blocking = $derived(references.filter((r) => !r.removable));
  /** A home beneath the one this file is, which declares the role: it cannot be removed from here. */
  const declaredBeneath = $derived.by(() => {
    if (mode.kind !== 'edit' || !file.home) return null;
    const order: WorkflowHome[] = ['defaults', 'project', 'local'];
    const here = order.indexOf(file.home);
    return (file.roleHomes[mode.id] ?? []).find((h) => order.indexOf(h) < here) ?? null;
  });

  /** The role as the form says it, keeping every key the form does not show as it was. */
  function role(): RoleSpec {
    const base: RoleSpec = original ? { ...original } : newRole();
    const chain = askFirst ? [] : falls.filter((f) => f !== '');
    const timeout =
      minutes === startMinutes
        ? start.timeout_ms
        : minutes.trim() === ''
          ? null
          : Math.round(Number(minutes) * 60_000);
    return {
      ...base,
      objective: objective.trim(),
      instructions: instructions.trim() === '' ? null : instructions,
      provider,
      model: model.trim() === '' ? null : model.trim(),
      effort: effort.trim() === '' ? null : effort.trim(),
      skills: splitList(skills),
      paths: splitList(paths),
      checks: chosenChecks,
      inputs,
      timeout_ms: Number.isFinite(timeout) ? timeout : null,
      retries,
      // One role keeps the one-role spelling, which a hand-written config may rely on: a single id
      // follows that role's own fallback in turn, and a list does not.
      fallback:
        chain.length === 0
          ? 'ask'
          : chain.length === 1 && !Array.isArray(original?.fallback)
            ? chain[0]!
            : chain,
    };
  }

  function chooseProvider(next: RoleProvider) {
    if (next === provider) return;
    provider = next;
    // Spelled the old provider's way; see the header.
    model = '';
    effort = '';
  }

  function toggleInput(input: RoleInput, on: boolean) {
    inputs = on ? [...inputs, input] : inputs.filter((i) => i !== input);
  }

  function toggleCheck(check: string, on: boolean) {
    chosenChecks = on ? [...chosenChecks, check] : chosenChecks.filter((c) => c !== check);
  }

  function setFallback(index: number, value: string) {
    const next = [...falls];
    next[index] = value;
    falls = next.filter((f, i) => f !== '' || i < next.length - 1);
  }

  /** The field of this role a refusal is about, or null when it is about something else. */
  function mine(refusal: RecipesRefusal | null, roleId: string): string | null {
    const at = errorAt(refusalKey(refusal));
    if (at?.at !== 'role' || at.role !== roleId) return null;
    return at.field ?? 'objective';
  }

  async function apply(event: Event) {
    event.preventDefault();
    if (busy) return;
    error = null;
    fieldErrors = {};
    const roleId = id.trim();
    if (mode.kind === 'new') {
      if (!ID_PATTERN.test(roleId)) {
        fieldErrors = { id: 'Letters, digits, underscores and hyphens only.' };
        return;
      }
      if (roleId in draft.role) {
        fieldErrors = { id: `There is already a role \`${roleId}\`.` };
        return;
      }
    }
    if (objective.trim() === '') {
      fieldErrors = {
        objective: 'A role needs an objective: it is the first line of its brief.',
      };
      return;
    }
    if (minutes !== startMinutes && minutes.trim() !== '' && !(Number(minutes) > 0)) {
      fieldErrors = { timeout_ms: 'A number of minutes, more than zero.' };
      return;
    }
    let next = putRole(draft, roleId, role());
    if (mode.kind === 'new') next = addToStage(next, mode.recipe, mode.stage, roleId);
    busy = true;
    try {
      // A project with nowhere chosen yet checks as the local layer would hold it: nothing else
      // declares a workflow, so where it goes changes nothing about whether it loads.
      await commands.checkRecipes(projectId, next, file.home ? null : 'local');
      onapply(next, null);
    } catch (e) {
      const refusal = refusalOf(e);
      const field = mine(refusal, roleId);
      if (field) {
        fieldErrors = { [field.split('.')[0]!]: errorMessage(e) };
      } else if (
        refusal &&
        (refusal.kind === 'invalid' || refusal.kind === 'unremovable')
      ) {
        // About another part of the draft: the role is fine, and the page shows the rest.
        onapply(next, refusal);
      } else {
        error = errorMessage(e);
      }
    } finally {
      busy = false;
    }
  }

  function remove() {
    if (mode.kind !== 'edit') return;
    onapply(deleteRole(draft, mode.id), null);
  }

  const shown = (field: string) => fieldErrors[field] ?? null;
  const FIELDS = [
    'id',
    'objective',
    'instructions',
    'provider',
    'model',
    'effort',
    'skills',
    'paths',
    'checks',
    'inputs',
    'timeout_ms',
    'retries',
    'fallback',
  ];
  const otherErrors = $derived(
    Object.entries(fieldErrors).filter(([field]) => !FIELDS.includes(field)),
  );
</script>

<Dialog {title} {onclose} onsubmit={apply} closeDisabled={busy} wide>
  {#snippet body()}
    <div class="o-stack c-role-form">
      {#if moved}
        <Banner variant="info">
          Moved from {providerLabel(moved.from)} to {providerLabel(moved.to)}. The model and
          effort were {providerLabel(moved.from)}'s, so {providerLabel(moved.to)} uses its own
          defaults unless you name them. Apply checks the change; Save writes it.
        </Banner>
      {/if}
      {#if error}<Banner><CodeText text={error} /></Banner>{/if}
      {#each otherErrors as [, message] (message)}<Banner
          ><CodeText text={message} /></Banner
        >{/each}

      {#if mode.kind === 'new'}
        <Field
          id="role-id"
          label="Role id"
          help="How recipes and fallbacks name it. Letters, digits, underscores and hyphens."
          errors={[shown('id')]}
        >
          <TextInput id="role-id" bind:value={id} mono placeholder="correctness" />
        </Field>
      {/if}

      <Field id="role-objective" label="Objective" errors={[shown('objective')]}>
        <TextInput
          id="role-objective"
          bind:value={objective}
          placeholder="Find bugs the change introduces"
        />
      </Field>
      <Field
        id="role-instructions"
        label="Instructions"
        help="Everything else the role should be told. Its brief adds the change, your rules and the repository's instructions as its inputs say."
        errors={[shown('instructions')]}
      >
        <textarea
          id="role-instructions"
          class="c-textarea"
          rows="3"
          bind:value={instructions}></textarea>
      </Field>

      <div class="c-role-form__grid">
        <Field id="role-provider" label="Provider" errors={[shown('provider')]}>
          <select
            id="role-provider"
            class="c-select"
            value={provider}
            onchange={(e) =>
              chooseProvider((e.currentTarget as HTMLSelectElement).value as RoleProvider)}
          >
            {#each PROVIDERS as option (option.id)}
              <option value={option.id}>{option.name}</option>
            {/each}
          </select>
        </Field>
        <Field
          id="role-model"
          label="Model"
          help="In the provider's own spelling. Empty uses its default."
          errors={[shown('model')]}
        >
          <TextInput
            id="role-model"
            bind:value={model}
            mono
            placeholder="Provider default"
          />
        </Field>
        <Field id="role-effort" label="Effort" errors={[shown('effort')]}>
          <TextInput
            id="role-effort"
            bind:value={effort}
            mono
            placeholder="Model default"
          />
        </Field>
      </div>

      <Field
        id="role-skills"
        label="Skills"
        help={`Comma-separated. Checked against ${providerLabel(provider)}'s own list when a review starts, and by Check recipe: providers don't share skills.`}
        errors={[shown('skills')]}
      >
        <TextInput id="role-skills" bind:value={skills} mono placeholder="/team-review" />
      </Field>
      <Field
        id="role-paths"
        label="Paths"
        help="Globs from the repository root. Empty means all changed files; a role whose paths match nothing changed is skipped."
        errors={[shown('paths')]}
      >
        <TextInput id="role-paths" bind:value={paths} mono placeholder="src/**/*.vue" />
      </Field>

      <fieldset class="c-role-form__set">
        <legend class="c-role-form__legend">Reads</legend>
        <div class="c-role-form__choices">
          {#each INPUTS as input (input.id)}
            <Choice
              size="sm"
              checked={inputs.includes(input.id)}
              onchange={(on) => toggleInput(input.id, on)}>{input.label}</Choice
            >
          {/each}
        </div>
        {#if shown('inputs')}<p class="c-field__help c-status--danger">
            <CodeText text={shown('inputs') ?? ''} />
          </p>{/if}
      </fieldset>

      <fieldset class="c-role-form__set">
        <legend class="c-role-form__legend">Checks it may run</legend>
        {#if checks.length === 0}
          <p class="c-field__help">
            No <code>[workflow.check]</code> is declared, so there is none to give it.
          </p>
        {:else}
          <div class="c-role-form__choices">
            {#each checks as check (check.id)}
              <Choice
                size="sm"
                checked={chosenChecks.includes(check.id)}
                onchange={(on) => toggleCheck(check.id, on)}>{check.label}</Choice
              >
            {/each}
          </div>
        {/if}
        {#if shown('checks')}<p class="c-field__help c-status--danger">
            <CodeText text={shown('checks') ?? ''} />
          </p>{/if}
      </fieldset>

      <div class="c-role-form__grid">
        <Field
          id="role-timeout"
          label="Timeout, minutes"
          help={`Empty is ${DEFAULT_TIMEOUT_MS / 60_000}.`}
          errors={[shown('timeout_ms')]}
        >
          <TextInput id="role-timeout" bind:value={minutes} placeholder="30" />
        </Field>
        <Field
          id="role-retries"
          label="Retries"
          help="Each is a whole turn against your usage window."
          errors={[shown('retries')]}
        >
          <select
            id="role-retries"
            class="c-select"
            value={String(retries)}
            onchange={(e) =>
              (retries = Number((e.currentTarget as HTMLSelectElement).value))}
          >
            {#each Array.from({ length: MAX_RETRIES + 1 }, (_, n) => n) as n (n)}
              <option value={String(n)}>{n === 0 ? 'None' : n}</option>
            {/each}
          </select>
        </Field>
      </div>

      <fieldset class="c-role-form__set">
        <legend class="c-role-form__legend">If it fails or reaches a usage limit</legend>
        <Choice
          type="radio"
          name="role-fallback"
          checked={askFirst}
          onchange={() => (askFirst = true)}>Pause and ask me</Choice
        >
        <Choice
          type="radio"
          name="role-fallback"
          checked={!askFirst}
          disabled={choices.length === 0}
          onchange={() => {
            askFirst = false;
            if (falls.length === 0 && choices[0]) falls = [choices[0]];
          }}>Switch to another role, then ask me</Choice
        >
        {#if !askFirst}
          <div class="c-role-form__chain">
            {#each Array.from({ length: Math.min(falls.length + 1, MAX_FALLBACKS) }, (_, i) => i) as i (i)}
              <select
                class="c-select"
                aria-label={`Fallback ${i + 1}`}
                value={falls[i] ?? ''}
                onchange={(e) =>
                  setFallback(i, (e.currentTarget as HTMLSelectElement).value)}
              >
                <option value="">{i === 0 ? 'Choose a role' : 'Nothing more'}</option>
                {#each choices as choice (choice)}
                  <option value={choice}
                    >{choice} · {providerLabel(
                      draft.role[choice]?.provider ?? 'claude',
                    )}</option
                  >
                {/each}
              </select>
            {/each}
          </div>
        {/if}
        <p class="c-field__help">
          {choices.length === 0
            ? 'No other role runs with the same profile and hands back the same kind of result, which a fallback must.'
            : 'A fallback starts from a written summary of where the run got to, usually with nobody watching.'}
        </p>
        {#if shown('fallback')}<p class="c-field__help c-status--danger">
            <CodeText text={shown('fallback') ?? ''} />
          </p>{/if}
      </fieldset>

      {#if original}
        <dl class="o-facts c-role-form__facts">
          <dt>Runs as</dt>
          <dd>
            {original.profile === 'writer'
              ? 'a writer: edits the worktree; commits stay the app’s'
              : 'a reviewer: reads, writes nothing'}{original.kind === 'runtime'
              ? ', exercising the running app'
              : ''}
          </dd>
          <dt>Hands back</dt>
          <dd>{original.output}</dd>
        </dl>
      {/if}

      {#if confirmingDelete}
        <div class="c-role-form__delete" role="alert">
          {#if blocking.length > 0}
            <p>
              <CodeText
                text={`\`${id}\` can't be deleted here: ${blocking.map((r) => r.text).join('; ')}. Change that in your config first.`}
              />
            </p>
          {:else if declaredBeneath}
            <p>
              <CodeText
                text={`\`${id}\` is declared in ${declaredBeneath === 'defaults' ? 'your defaults, [defaults.workflow]' : "this project's entry in your own config"}, beneath the file these recipes are saved to. Remove it there.`}
              />
            </p>
          {:else}
            <p>
              <CodeText
                text={references.length > 0
                  ? `Deleting \`${id}\` also takes it out of ${references.map((r) => r.text).join(', ')}.`
                  : `Delete \`${id}\`? Nothing else names it.`}
              />
            </p>
            <Button variant="danger-solid" size="sm" onclick={remove}>Delete role</Button>
          {/if}
          <Button size="sm" onclick={() => (confirmingDelete = false)}>Keep it</Button>
        </div>
      {/if}
    </div>
  {/snippet}
  {#snippet footer()}
    {#if mode.kind === 'edit' && !confirmingDelete}
      <span class="c-role-form__spacer">
        <Button
          variant="danger-outline"
          onclick={() => (confirmingDelete = true)}
          disabled={busy}>Delete role…</Button
        >
      </span>
    {/if}
    <Button onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="accent" type="submit" disabled={busy}
      >{busy ? 'Checking…' : 'Apply'}</Button
    >
  {/snippet}
</Dialog>
