<script lang="ts">
  /**
   * Settings › Recipes: one project's review recipes, the roles they run, and the roles that run by
   * themselves — planner, designer, implementer — edited here instead of in TOML.
   *
   * # One draft, written on Save
   *
   * Every edit — a role's fields, a role added, moved between stages or removed, a recipe added or
   * duplicated — changes a draft of the whole of `[workflow.role]` and `[workflow.recipe]`, and
   * nothing reaches the config until Save. Rust then writes only what differs, into the file that
   * already declares the workflow, keeping its comments and every other table (`recipes.rs` and
   * `wtm_config::workflow_file` say how). Discard goes back to what the file says. A run already
   * under way never sees the edit: it keeps the workflow it started with.
   *
   * # Errors where they are
   *
   * Save, Apply in the role editor and Check recipe all run the config's own validation on the
   * draft, and a refusal names a key (`role.conventions.retries`, `recipe.standard.stage[1].roles`):
   * that key decides where the message is drawn — on the role's card, under the stage, or at the
   * top when it is about neither.
   *
   * # Skills are the providers' to say
   *
   * A role's skills carry a mark only once Check recipe has asked the providers, the way a run's
   * session asks them, or when the Reviews tab opened this from a run that found one missing. Before
   * that a skill says "not checked yet" rather than looking fine. A mark belongs to the role as it
   * was checked: change its provider or skills and the mark goes until the next check.
   */
  import { untrack } from 'svelte';
  import { commands } from '../ipc/commands';
  import { errorMessage, type SkillProblem } from '../ipc/types';
  import {
    HOME_WORDS,
    ID_PATTERN,
    addToStage,
    agentLine,
    checkFromProblem,
    checkStillHolds,
    draftOf,
    duplicateRecipe,
    errorAt,
    fallbackLine,
    freeId,
    knowledgeLine,
    limitsLine,
    missingSkills,
    moveRole,
    moveStage,
    newRecipe,
    pathsLine,
    putRole,
    recipeLabel,
    refusalKey,
    refusalOf,
    removeFromRecipe,
    removeStage,
    rolesOf,
    sameDraft,
    skillMark,
    skillMarkWords,
    type RecipeSettingsView,
    type RecipesRefusal,
    type RoleSpec,
    type SkillCheckView,
    type WorkflowDraft,
    type WorkflowHome,
  } from '../recipes';
  import type { RecipesTarget } from '../state/settings.svelte';
  import RecipeRoleDialog, { type RoleDialogMode } from './RecipeRoleDialog.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  import Icon from './ui/Icon.svelte';
  import TextInput from './ui/TextInput.svelte';
  import CodeText from './CodeText.svelte';

  const {
    projectId,
    projectName,
    target = null,
  }: { projectId: string; projectName: string; target?: RecipesTarget | null } = $props();

  const EMPTY: WorkflowDraft = { role: {}, recipe: {} };
  /** The add-role and new-recipe pickers' "A new role…". */
  const NEW_ROLE = '__new';

  let view = $state<RecipeSettingsView | null>(null);
  let loadError = $state<string | null>(null);
  /** What the file says, and what the editor says. */
  let saved = $state<WorkflowDraft>(EMPTY);
  let draft = $state<WorkflowDraft>(EMPTY);
  let recipeId = $state<string | null>(null);

  let check = $state<SkillCheckView | null>(null);
  let checkedDraft = $state<WorkflowDraft | null>(null);
  /** The last validation passed for this draft: Check recipe's first half, or a save. */
  let validated = $state<WorkflowDraft | null>(null);

  let busy = $state<'save' | 'check' | null>(null);
  let refusal = $state<RecipesRefusal | null>(null);
  let message = $state<string | null>(null);
  let note = $state<string | null>(null);

  let editing = $state<RoleDialogMode | null>(null);
  let adding = $state(false);
  let addRole = $state(NEW_ROLE);
  let addStage = $state(0);
  let naming = $state<null | { kind: 'new' | 'duplicate'; role: string }>(null);
  let namingLabel = $state('');
  let choosingHome = $state(false);
  let home = $state<WorkflowHome>('local');

  const dirty = $derived(!sameDraft(draft, saved));
  const recipe = $derived(recipeId ? draft.recipe[recipeId] : undefined);
  const recipeIds = $derived(Object.keys(draft.recipe));
  const workflow = $derived(view?.workflow ?? null);
  const issue = $derived({
    planner: workflow?.planner ?? null,
    designer: workflow?.designer ?? null,
    implement: workflow?.implement ?? null,
  });
  const inRecipe = $derived(new Set(rolesOf(recipe).map((r) => r.role)));
  const issueRoles = $derived(
    [
      issue.planner ? { id: issue.planner, as: 'Planner' } : null,
      issue.designer ? { id: issue.designer, as: 'Designer' } : null,
      issue.implement ? { id: issue.implement.writer, as: 'Implementer' } : null,
    ].filter((r): r is { id: string; as: string } => r !== null && r.id in draft.role),
  );
  const otherRoles = $derived(
    Object.keys(draft.role).filter(
      (id) => !inRecipe.has(id) && !issueRoles.some((r) => r.id === id),
    ),
  );
  const writers = $derived(
    rolesOf(recipe)
      .map((r) => r.role)
      .filter((id) => draft.role[id]?.profile === 'writer'),
  );
  const at = $derived(errorAt(refusalKey(refusal)));
  /** A refusal not about any role or stage drawn here goes at the top. */
  const topMessage = $derived.by(() => {
    if (!message) return null;
    if (at?.at === 'role' && at.role in draft.role) return null;
    if (at?.at === 'stage' && at.recipe === recipeId) return null;
    return message;
  });

  async function load(project: string) {
    loadError = null;
    try {
      const next = await commands.recipeSettings(project);
      if (project !== projectId) return;
      take(next);
    } catch (e) {
      loadError = errorMessage(e);
    }
  }

  function take(next: RecipeSettingsView) {
    view = next;
    saved = draftOf(next.workflow);
    draft = draftOf(next.workflow);
    if (!recipeId || !(recipeId in draft.recipe))
      recipeId = Object.keys(draft.recipe)[0] ?? null;
  }

  $effect(() => {
    const project = projectId;
    untrack(() => {
      view = null;
      recipeId = target?.recipe ?? null;
      void load(project).then(() => untrack(() => arrive(target)));
    });
  });

  /** Opened from a run: its recipe, its role, and what it found missing. */
  function arrive(to: RecipesTarget | null) {
    if (!to || to.project !== projectId || !view) return;
    if (to.recipe && to.recipe in draft.recipe) recipeId = to.recipe;
    if (to.problem) {
      check = checkFromProblem(to.problem);
      checkedDraft = draftOf(draft);
    }
    if (to.role && to.role in draft.role) {
      editing = { kind: 'edit', id: to.role, provider: to.provider };
    }
  }

  function edit(next: WorkflowDraft) {
    draft = next;
    note = null;
    // A refusal described the draft before this edit.
    refusal = null;
    message = null;
  }

  function refused(e: unknown) {
    refusal = refusalOf(e);
    message = errorMessage(e);
  }

  function errorFor(role: string): string | null {
    return at?.at === 'role' && at.role === role ? message : null;
  }

  function stageError(stage: number): string | null {
    return at?.at === 'stage' && at.recipe === recipeId && at.stage === stage
      ? message
      : null;
  }

  function roleState(
    id: string,
  ): { words: string; tone: 'ok' | 'danger' | 'muted' } | null {
    const role = draft.role[id];
    if (!role) return null;
    if (errorFor(id)) return { words: 'Needs a fix', tone: 'danger' };
    const holds = !!check?.roles.includes(id) && checkStillHolds(checkedDraft, draft, id);
    if (holds && missingSkills(role, check).length > 0)
      return { words: 'Needs a fix', tone: 'danger' };
    if (JSON.stringify(role) !== JSON.stringify(saved.role[id]))
      return { words: 'Edited', tone: 'muted' };
    if (holds && validated && sameDraft(validated, draft))
      return { words: 'Ready', tone: 'ok' };
    return null;
  }

  function checkFor(id: string): SkillCheckView | null {
    return check?.roles.includes(id) && checkStillHolds(checkedDraft, draft, id)
      ? check
      : null;
  }

  function removeMissing(id: string) {
    const role = draft.role[id];
    if (!role) return;
    const missing = new Set(missingSkills(role, check));
    const next = putRole(draft, id, {
      ...role,
      skills: (role.skills ?? []).filter((s) => !missing.has(s)),
    });
    edit(next);
    // The check still describes the role's other skills.
    checkedDraft = draftOf(next);
  }

  async function checkRecipe() {
    if (busy || !recipeId || !view) return;
    busy = 'check';
    refusal = null;
    message = null;
    note = null;
    const asked = draftOf(draft);
    const project = projectId;
    try {
      await commands.checkRecipes(project, asked, view.file.home ? null : 'local');
      validated = asked;
      const found = await commands.checkRecipeSkills(project, asked, recipeId);
      if (project !== projectId) return;
      check = found;
      checkedDraft = asked;
      const unasked = found.providers.filter((p) => p.listed === null);
      note =
        found.problems.length === 0 && unasked.length === 0
          ? 'The recipe checks out: it would load, and every skill its roles name is listed by their provider.'
          : found.problems.length > 0
            ? `${found.problems.length === 1 ? 'One role' : `${found.problems.length} roles`} would not start: ${found.problems.map(problemWords).join('; ')}.`
            : null;
      if (unasked.length > 0) {
        message = unasked
          .map(
            (p) =>
              `${p.provider === 'claude' ? 'Claude' : 'Codex'} could not be asked: ${p.problem}`,
          )
          .join(' ');
      }
    } catch (e) {
      refused(e);
    } finally {
      busy = null;
    }
  }

  function problemWords(problem: SkillProblem): string {
    return problem.kind === 'missing'
      ? `\`${problem.role}\` names \`/${problem.skill}\`, which ${problem.provider === 'claude' ? 'Claude' : 'Codex'} does not list`
      : `\`${problem.role}\` could not be checked`;
  }

  async function save(chosen: WorkflowHome | null = null) {
    if (busy || !view) return;
    if (!view.file.home && !chosen) {
      choosingHome = true;
      return;
    }
    busy = 'save';
    refusal = null;
    message = null;
    note = null;
    const project = projectId;
    const sent = draftOf(draft);
    try {
      const next = await commands.saveRecipes(project, sent, view.file.revision, chosen);
      // Another project's tab by now: its own load is what it shows.
      if (project !== projectId) return;
      choosingHome = false;
      // An edit made while the save was in flight is kept, as an unsaved change against what was
      // just written, rather than overwritten by it.
      const later = sameDraft(draft, sent) ? null : draftOf(draft);
      take(next);
      if (later) draft = later;
      validated = sent;
      note = `Saved to ${placeWords(next.file.home)}. Runs started from now on use it; runs already under way keep the recipe they started with.`;
    } catch (e) {
      refused(e);
      if (refusalOf(e)?.kind === 'no_home') choosingHome = true;
    } finally {
      busy = null;
    }
  }

  function discard() {
    edit(draftOf(saved));
    if (recipeId && !(recipeId in draft.recipe))
      recipeId = Object.keys(draft.recipe)[0] ?? null;
  }

  /** "`[workflow]` in `/repo/.git/wtm.local.toml`", for `CodeText`. */
  function placeWords(of: WorkflowHome | null): string {
    const place = view?.places.find((p) => p.home === of);
    return place ? `\`${place.header}\` in \`${place.path}\`` : 'nowhere yet';
  }

  function nameRecipe() {
    if (!naming) return;
    const label = namingLabel.trim();
    const base =
      label
        .toLowerCase()
        .replace(/[^a-z0-9_-]+/g, '-')
        .replace(/^-+|-+$/g, '') || 'recipe';
    const id = freeId(ID_PATTERN.test(base) ? base : 'recipe', draft.recipe);
    if (naming.kind === 'duplicate' && recipeId) {
      edit(duplicateRecipe(draft, recipeId, id, label));
    } else if (naming.kind === 'new' && naming.role === NEW_ROLE) {
      // A recipe needs a role to be valid, and there is none to give it yet: the stage waits for
      // the role made next, and Discard takes both away.
      const next = draftOf(draft);
      next.recipe[id] = { label: label || null, stage: [{ roles: [] }], concurrency: null };
      edit(next);
      editing = { kind: 'new', recipe: id, stage: 0 };
    } else if (naming.kind === 'new' && naming.role) {
      edit(newRecipe(draft, id, label, naming.role));
    } else {
      return;
    }
    recipeId = id;
    naming = null;
  }

  function addToRecipe() {
    if (!recipeId) return;
    if (addRole === NEW_ROLE) {
      editing = { kind: 'new', recipe: recipeId, stage: addStage };
    } else {
      edit(addToStage(draft, recipeId, addStage, addRole));
    }
    adding = false;
  }

  function applied(next: WorkflowDraft, elsewhere: RecipesRefusal | null) {
    editing = null;
    edit(next);
    if (elsewhere) {
      refusal = elsewhere;
      message = 'message' in elsewhere ? elsewhere.message : null;
    }
  }

  function setLabel(value: string) {
    if (!recipeId || !recipe) return;
    const next = draftOf(draft);
    next.recipe[recipeId]!.label = value.trim() === '' ? null : value;
    edit(next);
  }

  function setConcurrency(value: string) {
    if (!recipeId || !recipe) return;
    const next = draftOf(draft);
    const n = Number(value);
    next.recipe[recipeId]!.concurrency =
      value.trim() === '' || !(n > 0) ? null : Math.floor(n);
    edit(next);
  }

  /** Whether a recipe this editor would delete is one something else needs. */
  const recipeInUse = $derived(issue.implement?.recipe === recipeId);
  function deleteRecipe() {
    if (!recipeId || recipeInUse) return;
    const next = draftOf(draft);
    delete next.recipe[recipeId];
    edit(next);
    recipeId = Object.keys(next.recipe)[0] ?? null;
  }
</script>

<div class="c-recipes">
  {#if loadError}
    <Banner>
      {loadError}
      {#snippet action()}
        <Button size="sm" onclick={() => void load(projectId)}>Retry</Button>
      {/snippet}
    </Banner>
  {:else if !view}
    <p class="c-recipes__empty">Reading this project's workflow…</p>
  {:else}
    <div class="c-recipes__head">
      <h3 class="c-recipes__title">Review recipes</h3>
      {#if recipeIds.length > 0}
        <label class="c-recipes__picker">
          Recipe
          <select
            class="c-select"
            value={recipeId ?? ''}
            onchange={(e) => (recipeId = (e.currentTarget as HTMLSelectElement).value)}
          >
            {#each recipeIds as id (id)}
              <option value={id}
                >{recipeLabel(id, draft.recipe[id])}{issue.implement?.recipe === id
                  ? ' (implementation review)'
                  : ''}</option
              >
            {/each}
          </select>
        </label>
        <Button
          size="sm"
          onclick={() => {
            namingLabel = `${recipeLabel(recipeId ?? '', recipe)} copy`;
            naming = { kind: 'duplicate', role: '' };
          }}>Duplicate</Button
        >
      {/if}
      <Button
        size="sm"
        onclick={() => {
          namingLabel = '';
          naming = { kind: 'new', role: Object.keys(draft.role)[0] ?? NEW_ROLE };
        }}>New recipe</Button
      >
    </div>

    <p class="c-recipes__where">
      {#if view.file.home}
        <CodeText
          text={`For ${projectName}. Saved to ${placeWords(view.file.home)}${view.file.home === 'defaults' ? ', which every project reads' : ''}.`}
        />
        {#if view.file.declaring.length > 1}
          <CodeText
            text={` Also declared beneath it: ${view.file.declaring
              .filter((h) => h !== view?.file.home)
              .map((h) => `\`${view?.places.find((p) => p.home === h)?.header ?? h}\``)
              .join(', ')}. What those declare can be changed here, not removed.`}
          />
        {/if}
      {:else}
        Nothing declares a workflow for {projectName} yet. Save asks where to keep it.
      {/if}
    </p>

    {#if topMessage}
      <Banner>
        <CodeText text={topMessage} />
        {#snippet action()}
          {#if refusal?.kind === 'stale'}
            <Button size="sm" onclick={() => void load(projectId)}>Reload</Button>
          {/if}
        {/snippet}
      </Banner>
    {/if}
    {#if note}<Banner variant="info"><CodeText text={note} /></Banner>{/if}

    {#if !recipeId || !recipe}
      <p class="c-recipes__empty">
        No review recipes yet. A recipe is stages of roles: the roles in a stage run
        together, and the next stage waits for them. New recipe makes one{Object.keys(
          draft.role,
        ).length === 0
          ? ', and its first role with it.'
          : ' from the roles below, or with a new one.'}
      </p>
    {:else}
      <div class="c-recipes__settings">
        <Field id="recipe-label" label="Name">
          <TextInput
            id="recipe-label"
            value={recipe.label ?? ''}
            placeholder={recipeId}
            oninput={(e) => setLabel((e.currentTarget as HTMLInputElement).value)}
          />
        </Field>
        <Field
          id="recipe-concurrency"
          label="Roles at once"
          help="Empty runs each stage whole."
          errors={[at?.at === 'recipe' && at.recipe === recipeId ? message : null]}
        >
          <TextInput
            id="recipe-concurrency"
            type="number"
            value={recipe.concurrency === null ? '' : String(recipe.concurrency)}
            oninput={(e) => setConcurrency((e.currentTarget as HTMLInputElement).value)}
          />
        </Field>
        <span class="c-recipes__id"><code>{recipeId}</code></span>
        <span class="c-recipes__delete">
          <Button
            variant="quiet"
            size="sm"
            disabled={recipeInUse}
            title={recipeInUse
              ? 'The implementer’s work is reviewed with this recipe (implement.recipe)'
              : undefined}
            onclick={deleteRecipe}>Delete recipe</Button
          >
        </span>
      </div>

      <div class="c-recipes__notice" role="note">
        <span class="c-recipes__notice-icon" aria-hidden="true"
          ><Icon name="lock" size={16} /></span
        >
        <p>
          {#if writers.length === 0}
            <strong>Every role runs as a read-only reviewer.</strong>
          {:else}
            <strong
              >Every role but <CodeText text={writers.map((w) => `\`${w}\``).join(', ')} /> runs
              as a read-only reviewer;</strong
            > a writer edits the worktree and wtm makes its commits.
          {/if}
          OS sandbox on, no network beyond package registries, no GitHub or tracker credentials,
          no MCP servers but wtm's run tools, and nothing from the repository's agent config.
          This is enforced when each session starts, and a canary proves the sandbox before it
          does — not asked for in the prompt.
        </p>
      </div>

      {#each recipe.stage as stage, index (index)}
        <section class="c-recipe-stage" aria-labelledby="recipe-stage-{index}">
          <div class="c-recipe-stage__head">
            <h4 class="c-recipe-stage__title" id="recipe-stage-{index}">
              Stage {index + 1}{stage.roles.length > 1 ? ' · in parallel' : ''}{index > 0
                ? ` · after stage ${index}`
                : ''}
            </h4>
            <Button
              variant="quiet"
              icon="sm"
              ariaLabel="Move stage {index + 1} earlier"
              disabled={index === 0}
              onclick={() => edit(moveStage(draft, recipeId ?? '', index, -1))}
              ><Icon name="chevron-up" /></Button
            >
            <Button
              variant="quiet"
              icon="sm"
              ariaLabel="Move stage {index + 1} later"
              disabled={index === recipe.stage.length - 1}
              onclick={() => edit(moveStage(draft, recipeId ?? '', index, 1))}
              ><Icon name="chevron-down" /></Button
            >
            <Button
              variant="quiet"
              icon="sm"
              ariaLabel="Remove stage {index + 1}"
              onclick={() => edit(removeStage(draft, recipeId ?? '', index))}
              ><Icon name="close" /></Button
            >
          </div>
          {#if stageError(index)}
            <p class="c-recipe-stage__error" role="alert">
              <CodeText text={stageError(index) ?? ''} />
            </p>
          {/if}
          {#if stage.roles.length === 0}
            <p class="c-recipe-stage__empty">
              No role in this stage yet. Add one, or remove the stage.
            </p>
          {/if}
          <div class="c-recipe-stage__roles">
            {#each stage.roles as id, position (id)}
              {@const number = rolesOf(recipe).findIndex((r) => r.role === id) + 1}
              {@render card(id, number, {
                stage: index,
                position,
                last:
                  index === recipe.stage.length - 1 && position === stage.roles.length - 1,
              })}
            {/each}
          </div>
        </section>
      {/each}

      {#if adding}
        <div class="c-recipes__add">
          <Field id="recipe-add-role" label="Role">
            <select
              id="recipe-add-role"
              class="c-select"
              value={addRole}
              onchange={(e) => (addRole = (e.currentTarget as HTMLSelectElement).value)}
            >
              <option value={NEW_ROLE}>A new role…</option>
              {#each Object.keys(draft.role).filter((id) => !inRecipe.has(id)) as id (id)}
                <option value={id}>{id} · {agentLine(draft.role[id]!)}</option>
              {/each}
            </select>
          </Field>
          <Field id="recipe-add-stage" label="Into">
            <select
              id="recipe-add-stage"
              class="c-select"
              value={String(addStage)}
              onchange={(e) =>
                (addStage = Number((e.currentTarget as HTMLSelectElement).value))}
            >
              {#each recipe.stage as _, index (index)}
                <option value={String(index)}>Stage {index + 1}, beside its roles</option>
              {/each}
              <option value={String(recipe.stage.length)}>A new stage after the last</option
              >
            </select>
          </Field>
          <div class="c-recipes__add-actions">
            <Button size="sm" onclick={() => (adding = false)}>Cancel</Button>
            <Button variant="accent" size="sm" onclick={addToRecipe}
              >{addRole === NEW_ROLE ? 'New role…' : 'Add'}</Button
            >
          </div>
        </div>
      {:else}
        <span class="c-recipes__add-button">
          <Button
            variant="dashed"
            onclick={() => {
              addRole = NEW_ROLE;
              addStage = recipe?.stage.length ?? 0;
              adding = true;
            }}>Add a role or stage</Button
          >
        </span>
      {/if}
    {/if}

    {#if issueRoles.length > 0}
      <section class="c-recipe-stage" aria-labelledby="recipe-issue-roles">
        <div class="c-recipe-stage__head">
          <h4 class="c-recipe-stage__title" id="recipe-issue-roles">
            Roles that run by themselves
          </h4>
        </div>
        <p class="c-recipe-stage__empty">
          Named by <code>planner</code>, <code>designer</code> and
          <code>implement.writer</code> in your config, which this tab does not change. Their
          roles are edited here like any other.
        </p>
        <div class="c-recipe-stage__roles">
          {#each issueRoles as entry (entry.as)}
            {@render card(entry.id, null, null, entry.as)}
          {/each}
        </div>
      </section>
    {/if}

    {#if otherRoles.length > 0}
      <section class="c-recipe-stage" aria-labelledby="recipe-other-roles">
        <div class="c-recipe-stage__head">
          <h4 class="c-recipe-stage__title" id="recipe-other-roles">
            Roles {recipe ? 'this recipe does not run' : ''}
          </h4>
        </div>
        <ul class="o-plain-list c-recipes__others">
          {#each otherRoles as id (id)}
            <li class="c-recipes__other">
              <code>{id}</code>
              <span class="c-recipes__other-line"
                >{agentLine(draft.role[id]!)} · {draft.role[id]!.objective}</span
              >
              {#if errorFor(id)}<span class="c-status--danger"
                  ><CodeText text={errorFor(id) ?? ''} /></span
                >{/if}
              <Button
                size="sm"
                onclick={() => (editing = { kind: 'edit', id, provider: null })}
                >Edit role</Button
              >
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    <div class="c-recipes__foot">
      <p class="c-recipes__foot-note">
        {#if recipe}
          {recipe.concurrency
            ? `Up to ${recipe.concurrency} roles at once`
            : 'Each stage runs whole'}
          · each role stops at its own limit · findings from all roles are merged by file, line
          range and rule
        {/if}
      </p>
      {#if dirty}
        <Button variant="quiet" disabled={busy !== null} onclick={discard}
          >Discard changes</Button
        >
      {/if}
      <Button disabled={busy !== null || !recipeId} onclick={() => void checkRecipe()}
        >{busy === 'check' ? 'Checking…' : 'Check recipe'}</Button
      >
      <Button
        variant="accent"
        disabled={busy !== null || !dirty}
        onclick={() => void save()}>{busy === 'save' ? 'Saving…' : 'Save'}</Button
      >
    </div>
  {/if}
</div>

{#snippet card(
  id: string,
  number: number | null,
  place: { stage: number; position: number; last: boolean } | null,
  as: string | null = null,
)}
  {@const role = draft.role[id] as RoleSpec | undefined}
  {#if role}
    {@const state = roleState(id)}
    {@const checked = checkFor(id)}
    {@const missing = missingSkills(role, checked)}
    <article
      class="c-recipe-role"
      class:is-failing={state?.tone === 'danger'}
      id="recipe-role-{id}"
      aria-labelledby="recipe-role-title-{id}"
    >
      <div class="c-recipe-role__head">
        {#if number !== null}<span class="c-recipe-role__number">{number}</span>{/if}
        <h5 class="c-recipe-role__title" id="recipe-role-title-{id}">
          {as ? `${as} · ` : ''}{id}
        </h5>
        {#if state}
          <span class="c-recipe-role__state c-recipe-role__state--{state.tone}"
            >{state.words}</span
          >
        {/if}
      </div>
      <p class="c-recipe-role__objective">{role.objective}</p>
      <dl class="o-facts c-recipe-role__facts">
        <dt>Agent</dt>
        <dd>{agentLine(role)}</dd>
        <dt>Skills</dt>
        <dd class="c-recipe-role__skills">
          {#each role.skills ?? [] as skill (skill)}
            {@const mark = skillMark(role, skill, checked)}
            <span class="c-recipe-role__skill">
              <code>/{skill.replace(/^\/+/, '')}</code>
              <span class="c-recipe-role__mark c-recipe-role__mark--{mark}"
                >{skillMarkWords(mark, role.provider)}</span
              >
            </span>
          {:else}
            <span class="c-recipe-role__none">None</span>
          {/each}
        </dd>
        <dt>Knowledge</dt>
        <dd>{knowledgeLine(role)}</dd>
        <dt>Paths</dt>
        <dd class:c-recipe-role__mono={(role.paths ?? []).length > 0}>{pathsLine(role)}</dd>
        {#if (role.checks ?? []).length > 0}
          <dt>Checks</dt>
          <dd>{(role.checks ?? []).join(', ')}</dd>
        {/if}
        <dt>Limits</dt>
        <dd>{limitsLine(role)}</dd>
        <dt>If limited</dt>
        <dd><CodeText text={fallbackLine(role, draft.role)} /></dd>
      </dl>
      {#if errorFor(id)}
        <p class="c-recipe-role__error" role="alert">
          <CodeText text={errorFor(id) ?? ''} />
        </p>
      {/if}
      <div class="c-recipe-role__actions">
        {#if missing.length > 0}
          <Button variant="accent" size="sm" onclick={() => removeMissing(id)}
            >Remove {missing.length === 1 ? 'skill' : 'skills'} from this role</Button
          >
        {/if}
        <Button size="sm" onclick={() => (editing = { kind: 'edit', id, provider: null })}
          >Edit role</Button
        >
        {#if place && recipeId}
          {@const rid = recipeId}
          <span class="c-recipe-role__order">
            <Button
              variant="quiet"
              icon="sm"
              ariaLabel="Move {id} earlier"
              disabled={place.stage === 0 && place.position === 0}
              onclick={() => edit(moveRole(draft, rid, place.stage, id, -1))}
              ><Icon name="chevron-left" /></Button
            >
            <Button
              variant="quiet"
              icon="sm"
              ariaLabel="Move {id} later"
              disabled={place.last}
              onclick={() => edit(moveRole(draft, rid, place.stage, id, 1))}
              ><Icon name="chevron-right" /></Button
            >
            <Button
              variant="quiet"
              icon="sm"
              ariaLabel="Remove {id} from this recipe"
              onclick={() => edit(removeFromRecipe(draft, rid, place.stage, id))}
              ><Icon name="close" /></Button
            >
          </span>
        {/if}
      </div>
    </article>
  {/if}
{/snippet}

{#if editing && view}
  <RecipeRoleDialog
    {projectId}
    mode={editing}
    {draft}
    file={view.file}
    {issue}
    checks={view.workflow.checks}
    onapply={applied}
    onclose={() => (editing = null)}
  />
{/if}

{#if naming}
  <Dialog
    title={naming.kind === 'new' ? 'New recipe' : 'Duplicate recipe'}
    onclose={() => (naming = null)}
    onsubmit={(e) => {
      e.preventDefault();
      nameRecipe();
    }}
  >
    {#snippet body()}
      <div class="o-stack">
        <Field id="recipe-name" label="Name">
          <TextInput id="recipe-name" bind:value={namingLabel} placeholder="Quick" />
        </Field>
        {#if naming?.kind === 'new'}
          <Field
            id="recipe-first-role"
            label="First role"
            help="A recipe needs one to run; add the rest after."
          >
            <select
              id="recipe-first-role"
              class="c-select"
              value={naming.role}
              onchange={(e) =>
                naming && (naming.role = (e.currentTarget as HTMLSelectElement).value)}
            >
              <option value={NEW_ROLE}>A new role…</option>
              {#each Object.keys(draft.role) as id (id)}
                <option value={id}>{id} · {agentLine(draft.role[id]!)}</option>
              {/each}
            </select>
          </Field>
        {/if}
      </div>
    {/snippet}
    {#snippet footer()}
      <Button onclick={() => (naming = null)}>Cancel</Button>
      <Button variant="accent" type="submit" disabled={!namingLabel.trim()}>
        {naming?.kind === 'new' ? 'Make recipe' : 'Duplicate'}
      </Button>
    {/snippet}
  </Dialog>
{/if}

{#if choosingHome && view}
  <Dialog
    title="Where should these recipes live?"
    onclose={() => (choosingHome = false)}
    closeDisabled={busy !== null}
    onsubmit={(e) => {
      e.preventDefault();
      void save(home);
    }}
  >
    {#snippet body()}
      <div class="o-stack">
        <p>
          Nothing declares a workflow for {projectName} yet. Never the repository's
          <code>wtm.toml</code>: a role decides which of your accounts and tools a run uses.
        </p>
        {#each ['local', 'project', 'defaults'] as const as option (option)}
          {@const place = view?.places.find((p) => p.home === option)}
          <Choice
            type="radio"
            name="recipe-home"
            checked={home === option}
            onchange={() => (home = option)}
          >
            <strong>{HOME_WORDS[option].label}</strong>
            <span class="c-recipes__home-help"
              >{HOME_WORDS[option].help} <code>{place?.header}</code> in
              <code>{place?.path}</code></span
            >
          </Choice>
        {/each}
      </div>
    {/snippet}
    {#snippet footer()}
      <Button onclick={() => (choosingHome = false)} disabled={busy !== null}>Cancel</Button
      >
      <Button variant="accent" type="submit" disabled={busy !== null}
        >{busy === 'save' ? 'Saving…' : 'Save here'}</Button
      >
    {/snippet}
  </Dialog>
{/if}
