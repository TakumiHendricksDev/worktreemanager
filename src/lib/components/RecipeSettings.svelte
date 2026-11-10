<script lang="ts">
  /**
   * Settings › Recipes: the list on the left, the selected recipe's editor on the right.
   *
   * # Saved with a button, unlike the rest of Settings
   *
   * Everything else in Settings applies the moment it changes, because a palette you cannot see
   * until you confirm it is a palette you cannot choose. A recipe is the exception that rule's own
   * reasoning makes: the moment a recipe exists, an action can offer it and someone can run it, so a
   * three-step recipe saved after its first keystroke is a runnable recipe with two empty steps. The
   * draft below is the only copy that changes until Save, and the backend refuses a save with
   * problems rather than storing it.
   *
   * `ondirty` tells Settings whether there is anything to lose, so closing can ask first.
   */
  import { commands } from '../ipc/commands';
  import {
    errorMessage,
    type AgentOption,
    type AgentSkill,
    type Recipe,
    type RecipeStep,
  } from '../ipc/types';
  import {
    NEW_RECIPE,
    RECIPE_ACTIONS,
    TEMPLATES,
    agentMark,
    blankRecipe,
    blankStep,
  } from '../recipes';
  import { recipes } from '../state/recipes.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import { workspace } from '../state/workspace.svelte';
  import RecipeStepCard from './RecipeStepCard.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Icon from './ui/Icon.svelte';

  const { ondirty }: { ondirty: (dirty: boolean) => void } = $props();

  /** The catalogue, whatever the active repository offers: a recipe is not tied to one. */
  const FALLBACK: AgentOption[] = [
    { id: 'claude', label: 'Claude Code' },
    { id: 'codex', label: 'Codex' },
    { id: 'cursor', label: 'Cursor Agent' },
  ].map((a) => ({
    ...a,
    blurb: '',
    available: true,
    offered: true,
    detail: null,
    model: null,
    effort: null,
    mode: null,
  }));
  const agents = $derived(sessions.options.length > 0 ? sessions.options : FALLBACK);

  let selectedId = $state<string | null>(null);
  let draft = $state<Recipe | null>(null);
  /** The draft as it was when loaded or last saved, to tell whether anything changed. */
  let baseline = $state('');
  let openStep = $state(0);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let confirm = $state<{ kind: 'leave'; then: () => void } | { kind: 'delete' } | null>(
    null,
  );

  const dirty = $derived(draft !== null && JSON.stringify(draft) !== baseline);
  $effect(() => ondirty(dirty));

  function edit(recipe: Recipe | null, step = 0) {
    draft = recipe ? structuredClone($state.snapshot(recipe)) : null;
    baseline = JSON.stringify(draft);
    selectedId = recipe?.id || null;
    openStep = step;
    error = null;
  }

  /** Select what Settings was opened for, or the first recipe, once the list has loaded. */
  $effect(() => {
    if (!recipes.loaded || draft !== null) return;
    if (settingsView.focus === NEW_RECIPE) {
      // Asked for from Actions, where there was nothing left to offer: open on a new one.
      draft = blankRecipe();
      baseline = '';
      return;
    }
    const wanted = settingsView.focus ? recipes.byId(settingsView.focus) : undefined;
    edit(wanted ?? recipes.recipes[0] ?? null);
  });

  /** Run `then` now, or after asking, when the draft has changes it would throw away. */
  function leave(then: () => void) {
    if (dirty) confirm = { kind: 'leave', then };
    else then();
  }

  function select(id: string) {
    if (id === selectedId && draft?.id === id) return;
    leave(() => edit(recipes.byId(id) ?? null));
  }

  function startNew(from?: () => Recipe) {
    leave(() => {
      const recipe = from ? from() : blankRecipe();
      draft = recipe;
      // A new recipe is unsaved by definition, so the baseline is nothing at all.
      baseline = '';
      selectedId = null;
      openStep = 0;
      error = null;
    });
  }

  function duplicate() {
    if (!draft) return;
    const copy = structuredClone($state.snapshot(draft));
    startNew(() => ({ ...copy, id: '', name: `${copy.name} copy` }));
  }

  async function save() {
    if (!draft || saving) return;
    saving = true;
    error = null;
    try {
      const id = await recipes.save($state.snapshot(draft));
      edit(recipes.byId(id) ?? null, openStep);
    } catch (e) {
      error = errorMessage(e);
    } finally {
      saving = false;
    }
  }

  function discard() {
    const saved = draft?.id ? recipes.byId(draft.id) : undefined;
    edit(saved ?? recipes.recipes[0] ?? null);
  }

  async function remove() {
    confirm = null;
    if (!draft) return;
    if (draft.id) {
      try {
        await recipes.remove(draft.id);
      } catch (e) {
        error = errorMessage(e);
        return;
      }
    }
    edit(recipes.recipes[0] ?? null);
  }

  function patchStep(index: number, patch: Partial<RecipeStep>) {
    const step = draft?.steps[index];
    if (step) Object.assign(step, patch);
  }

  function move(index: number, by: -1 | 1) {
    if (!draft) return;
    const steps = draft.steps;
    const to = index + by;
    const a = steps[index];
    const b = steps[to];
    if (!a || !b) return;
    steps[index] = b;
    steps[to] = a;
    // A step that ends up first has no conversation before it to continue.
    if (steps[0]) steps[0].conversation = 'new';
    openStep = to;
  }

  function duplicateStep(index: number) {
    const step = draft?.steps[index];
    if (!draft || !step) return;
    const copy = {
      ...$state.snapshot(step),
      name: `${step.name} (copy)`,
      conversation: 'new' as const,
    };
    draft.steps.splice(index + 1, 0, copy);
    openStep = index + 1;
  }

  function removeStep(index: number) {
    if (!draft || draft.steps.length === 1) return;
    draft.steps.splice(index, 1);
    if (draft.steps[0]) draft.steps[0].conversation = 'new';
    openStep = Math.max(0, index - 1);
  }

  function addStep() {
    if (!draft) return;
    const last = draft.steps.at(-1);
    draft.steps.push({ ...blankStep(last?.agent), name: `Step ${draft.steps.length + 1}` });
    openStep = draft.steps.length - 1;
  }

  /**
   * Skills per agent, for the worktree in view: what is on disk at once, then what the agent's own
   * CLI reports, which replaces it.
   *
   * Two reads because the second is the only one with a plugin's skills in it, and takes a few
   * seconds — it opens the CLI the way a pane does and asks, without a turn. The disk list is what
   * the picker shows meanwhile, and what stays if the CLI cannot be asked. Read once per agent while
   * Settings is open; `asked` is a plain set so the effect that fills `skills` does not also depend
   * on it and run again for its own write.
   */
  let skills = $state<Record<string, AgentSkill[] | null>>({});
  let asking = $state<Record<string, boolean>>({});
  const asked = new Set<string>();
  /** Where skills are asked about: the worktree being looked at, which is where plugins are on. */
  const place = $derived(
    workspace.selected?.dirname ?? workspace.activeProject?.name ?? 'your home directory',
  );
  $effect(() => {
    const projectId = workspace.activeProjectId;
    const worktreeId = workspace.selectedWorktreeId;
    for (const step of draft?.steps ?? []) {
      if (step.kind !== 'skill' || asked.has(step.agent)) continue;
      const agent = step.agent;
      asked.add(agent);
      asking[agent] = true;
      commands
        .agentSkills(agent, projectId, worktreeId)
        .then((list) => {
          // Not over an answer from the CLI that happened to arrive first.
          if (asking[agent]) skills[agent] = list;
        })
        .catch(() => {
          skills[agent] ??= [];
        });
      commands
        .probeAgentSkills(agent, projectId, worktreeId)
        .then((list) => {
          if (list !== null) skills[agent] = list;
        })
        .catch(() => {})
        .finally(() => {
          asking[agent] = false;
          skills[agent] ??= [];
        });
    }
  });

  /**
   * The New Worktree form's fields for the repository in view, so the editor offers the `form.*`
   * variables that exist and can say when a step names one that does not. Null until read, and for
   * a repository whose form could not be read: then nothing is flagged rather than everything.
   */
  let formFields = $state<{ key: string; label: string }[] | null>(null);
  $effect(() => {
    const projectId = workspace.activeProjectId;
    formFields = null;
    if (!projectId) return;
    commands
      .worktreeForm(projectId)
      .then((form) => {
        if (workspace.activeProjectId === projectId)
          formFields = form.fields.map((f) => ({ key: f.key, label: f.label }));
      })
      .catch(() => {});
  });

  const offeredOn = (id: string) =>
    RECIPE_ACTIONS.filter((action) => (recipes.offers[action.id] ?? []).includes(id));

  function agentsOf(recipe: Recipe): string[] {
    return [...new Set(recipe.steps.map((s) => s.agent))];
  }

  const limited = $derived((draft?.repositories.length ?? 0) > 0);
  const addable = $derived(
    workspace.projects.filter((p) => !(draft?.repositories ?? []).includes(p.id)),
  );
  const projectName = (id: string) =>
    workspace.projects.find((p) => p.id === id)?.name ?? id.split('/').pop() ?? id;

  function setScope(value: string) {
    if (!draft) return;
    if (value === 'all') draft.repositories = [];
    else if (draft.repositories.length === 0) {
      const first = workspace.activeProjectId ?? workspace.projects[0]?.id;
      if (first) draft.repositories = [first];
    }
  }
</script>

<div class="c-recipe-list" data-od-id="recipe-list">
  <div class="c-recipe-list__top">
    <Button variant="neutral" size="sm" full onclick={() => startNew()}>
      <Icon name="plus" size={12} /> New recipe
    </Button>
  </div>
  <div class="c-recipe-list__items" role="listbox" aria-label="Recipes">
    {#if draft && !draft.id}
      <button
        type="button"
        role="option"
        aria-selected="true"
        class="c-recipe-item is-selected"
      >
        <span class="c-recipe-item__name">
          {draft.name || 'Untitled recipe'}<span
            class="c-recipe-item__dirty"
            title="Not saved yet"
          ></span>
        </span>
        <span class="c-recipe-item__meta">Not saved yet</span>
      </button>
    {/if}
    {#each recipes.recipes as recipe (recipe.id)}
      {@const selected = draft?.id === recipe.id}
      {@const on = offeredOn(recipe.id)}
      <button
        type="button"
        role="option"
        aria-selected={selected}
        class="c-recipe-item"
        class:is-selected={selected}
        onclick={() => select(recipe.id)}
      >
        <span class="c-recipe-item__name">
          {recipe.name}
          {#if selected && dirty}<span class="c-recipe-item__dirty" title="Unsaved changes"
            ></span>{/if}
        </span>
        <span class="c-recipe-item__meta">
          {#each agentsOf(recipe) as agent (agent)}
            <span class="c-agent-mark c-agent-mark--{agent}" aria-hidden="true"
              >{agentMark(agent)}</span
            >
          {/each}
          <span>{recipe.steps.length} step{recipe.steps.length === 1 ? '' : 's'}</span>
          {#if on[0]}
            <span class="c-recipe-item__on"
              >Offered on {on[0].name}{on.length > 1 ? ` +${on.length - 1}` : ''}</span
            >
          {/if}
        </span>
      </button>
    {/each}
    {#each recipes.problems as problem (problem.id)}
      <p class="c-recipe-list__problem c-status--warn" title={problem.message}>
        <code>{problem.id}</code> in your config.toml could not be read: {problem.message}
      </p>
    {/each}
  </div>
  <p class="c-recipe-list__foot">
    Start from a template:
    {#each TEMPLATES as template, i (template.id)}{#if i > 0},
      {/if}<Button variant="link" size="sm" onclick={() => startNew(template.build)}
        >{template.build().name}</Button
      >{/each}.
  </p>
</div>

<div class="c-recipe" data-od-id="recipe-editor">
  {#if draft}
    <div class="c-recipe__scroll">
      <div class="c-recipe__inner">
        <div class="c-recipe__switch">
          <select
            class="c-select"
            aria-label="Recipe"
            value={draft.id}
            onchange={(e) => select((e.currentTarget as HTMLSelectElement).value)}
          >
            {#if !draft.id}<option value="">{draft.name || 'Untitled recipe'}</option>{/if}
            {#each recipes.recipes as recipe (recipe.id)}
              <option value={recipe.id}>{recipe.name}</option>
            {/each}
          </select>
          <Button variant="neutral" size="sm" onclick={() => startNew()}>
            <Icon name="plus" size={12} /> New
          </Button>
        </div>

        <input
          class="c-recipe__name"
          aria-label="Recipe name"
          bind:value={draft.name}
          placeholder="Name this recipe"
        />
        <input
          class="c-recipe__desc"
          aria-label="Description"
          bind:value={draft.description}
          placeholder="What it’s for"
        />

        <dl class="c-recipe__facts">
          <dt>Repositories</dt>
          <dd>
            <select
              class="c-select"
              aria-label="Repositories"
              value={limited ? 'some' : 'all'}
              onchange={(e) => setScope((e.currentTarget as HTMLSelectElement).value)}
            >
              <option value="all">Every repository</option>
              <option value="some">Only some…</option>
            </select>
            {#if limited}
              {#each draft.repositories as root (root)}
                <span class="c-chip">
                  {projectName(root)}
                  <button
                    type="button"
                    class="c-chip__remove"
                    aria-label="Stop offering it in {projectName(root)}"
                    onclick={() =>
                      draft &&
                      (draft.repositories = draft.repositories.filter((r) => r !== root))}
                  >
                    <Icon name="close" size={12} />
                  </button>
                </span>
              {/each}
              {#if addable.length > 0}
                <select
                  class="c-select"
                  aria-label="Add a repository"
                  value=""
                  onchange={(e) => {
                    const value = (e.currentTarget as HTMLSelectElement).value;
                    if (value && draft) draft.repositories = [...draft.repositories, value];
                  }}
                >
                  <option value="">Add…</option>
                  {#each addable as project (project.id)}
                    <option value={project.id}>{project.name}</option>
                  {/each}
                </select>
              {/if}
            {/if}
          </dd>
          <dt>Offered on</dt>
          <dd>
            {#if draft.id && offeredOn(draft.id).length > 0}
              {#each offeredOn(draft.id) as action (action.id)}
                <span class="c-chip c-chip--plain">{action.name}</span>
              {/each}
            {:else}
              <span class="c-status--muted">No action offers it yet</span>
            {/if}
            <Button
              variant="link"
              size="sm"
              onclick={() => leave(() => settingsView.show('actions'))}
              >Change in Actions</Button
            >
          </dd>
          <dt>By hand</dt>
          <dd class="c-status--muted">
            A worktree’s right-click menu › Run recipe › {draft.name || 'this recipe'}
          </dd>
        </dl>

        <div class="c-recipe__steps-head">
          <h3 class="c-section-heading">Steps</h3>
          <span class="c-recipe__flow">Each step waits for the one before it</span>
        </div>
        <ol class="c-recipe-steps" data-od-id="recipe-steps">
          {#each draft.steps as step, index (index)}
            <RecipeStepCard
              {step}
              {index}
              steps={draft.steps}
              open={openStep === index}
              {agents}
              skills={skills[step.agent] ?? null}
              asking={asking[step.agent] ?? false}
              {place}
              {formFields}
              projectName={workspace.activeProject?.name ?? null}
              ontoggle={() => (openStep = openStep === index ? -1 : index)}
              onchange={(patch) => patchStep(index, patch)}
              onmove={(by) => move(index, by)}
              onduplicate={() => duplicateStep(index)}
              onremove={() => removeStep(index)}
            />
          {/each}
          <li class="c-recipe-step">
            <div class="c-recipe-step__rail"></div>
            <button type="button" class="c-recipe-steps__add" onclick={addStep}>
              <Icon name="plus" size={14} /> Add a step
            </button>
          </li>
        </ol>
      </div>
    </div>

    <div class="c-recipe__savebar" data-od-id="recipe-savebar">
      {#if error}
        <span class="c-recipe__status c-status--danger" role="alert">{error}</span>
      {:else if dirty}
        <span class="c-recipe__status is-dirty"
          >{draft.id
            ? `Unsaved changes to ${draft.name || 'this recipe'}`
            : 'Not saved yet'}</span
        >
      {:else}
        <span class="c-recipe__status">Saved</span>
      {/if}
      <Button variant="quiet" size="sm" disabled={!draft.id} onclick={duplicate}
        >Duplicate</Button
      >
      <Button variant="quiet" size="sm" onclick={() => (confirm = { kind: 'delete' })}
        >Delete</Button
      >
      <Button variant="neutral" disabled={!dirty || saving} onclick={discard}
        >Discard</Button
      >
      <Button variant="accent" disabled={!dirty || saving} onclick={() => void save()}
        >{saving ? 'Saving…' : 'Save recipe'}</Button
      >
    </div>
  {:else}
    <div class="c-recipe__empty">
      <h3 class="c-recipe__empty-title">No recipes yet</h3>
      <p class="c-field__help">
        A recipe is steps that run one after another, each on the agent and model you pick.
        Start from nothing or from a template.
      </p>
      <div class="c-recipe__empty-actions">
        <Button variant="accent" onclick={() => startNew()}>New recipe</Button>
        {#each TEMPLATES as template (template.id)}
          <Button variant="neutral" onclick={() => startNew(template.build)}
            >{template.build().name}</Button
          >
        {/each}
      </div>
    </div>
  {/if}
</div>

{#if confirm?.kind === 'leave'}
  {@const then = confirm.then}
  <Dialog title="Discard unsaved changes?" onclose={() => (confirm = null)}>
    {#snippet body()}
      <p>
        {draft?.name || 'This recipe'} has changes that aren’t saved. Leaving it throws them away.
      </p>
    {/snippet}
    {#snippet footer()}
      <Button variant="neutral" onclick={() => (confirm = null)}>Keep editing</Button>
      <Button
        variant="danger-solid"
        onclick={() => {
          // Read before clearing, for the reason SettingsDialog's Discard does: `then` is derived
          // from `confirm`, and is gone the moment `confirm` is.
          const go = then;
          confirm = null;
          // Back to the saved copy first, so whatever `then` opens is not asked about again.
          baseline = JSON.stringify(draft);
          go();
        }}>Discard</Button
      >
    {/snippet}
  </Dialog>
{:else if confirm?.kind === 'delete'}
  <Dialog title="Delete {draft?.name || 'this recipe'}?" onclose={() => (confirm = null)}>
    {#snippet body()}
      <p>
        It is removed from your config.toml, and from every action that offers it. Runs
        already going carry on with the copy they started with.
      </p>
    {/snippet}
    {#snippet footer()}
      <Button variant="neutral" onclick={() => (confirm = null)}>Cancel</Button>
      <Button variant="danger-solid" onclick={() => void remove()}>Delete</Button>
    {/snippet}
  </Dialog>
{/if}
