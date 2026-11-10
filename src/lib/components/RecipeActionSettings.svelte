<script lang="ts">
  /**
   * Settings › Actions: which recipes each moment in wtm offers.
   *
   * Unlike recipes, a binding applies the moment it changes, like the rest of Settings. Offering a
   * saved recipe is one click that is just as easily undone, and it runs nothing — the action still
   * asks every time.
   */
  import {
    NEW_RECIPE,
    PLACE_VARIABLES,
    RECIPE_ACTIONS,
    type RecipeActionId,
  } from '../recipes';
  import { recipes } from '../state/recipes.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import { workspace } from '../state/workspace.svelte';
  import RecipeOffer from './RecipeOffer.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  let selected = $state<RecipeActionId>(
    RECIPE_ACTIONS.find((a) => a.id === settingsView.focus)?.id ?? 'worktree.created',
  );
  const action = $derived(
    RECIPE_ACTIONS.find((a) => a.id === selected) ?? RECIPE_ACTIONS[0]!,
  );
  const ids = $derived(recipes.offers[action.id] ?? []);
  const bound = $derived(ids.map((id) => recipes.byId(id)).filter((r) => r !== undefined));
  const unbound = $derived(recipes.recipes.filter((r) => !ids.includes(r.id)));
  let adding = $state('');
  /** What Offer adds: the one picked, or the first left — never a blank the select cannot show. */
  const toAdd = $derived(
    unbound.some((r) => r.id === adding) ? adding : (unbound[0]?.id ?? ''),
  );

  function namesFor(id: string): string {
    const names = (recipes.offers[id] ?? [])
      .map((r) => recipes.byId(r)?.name)
      .filter((n): n is string => n !== undefined);
    return names.length > 0 ? names.join(', ') : 'Nothing offered — works as today';
  }

  function asks(id: string): boolean {
    return (recipes.offers[id] ?? []).some((r) => recipes.byId(r) !== undefined);
  }

  function move(index: number, by: -1 | 1) {
    const next = [...ids];
    const a = next[index];
    const b = next[index + by];
    if (a === undefined || b === undefined) return;
    next[index] = b;
    next[index + by] = a;
    void recipes.setOffers(action.id, next);
  }

  function offer() {
    const id = toAdd;
    if (!id) return;
    void recipes.setOffers(action.id, [...ids, id]);
    adding = '';
  }

  const place = $derived(
    action.id === 'project.added'
      ? `the main checkout of ${workspace.activeProject?.name ?? 'the repository'}`
      : (workspace.selected?.dirname ?? 'feature-branch'),
  );
</script>

<div class="c-actions" role="listbox" aria-label="Actions" data-od-id="action-list">
  {#each RECIPE_ACTIONS as item (item.id)}
    <button
      type="button"
      role="option"
      aria-selected={item.id === selected}
      class="c-action-item"
      class:is-selected={item.id === selected}
      class:is-bound={asks(item.id)}
      onclick={() => (selected = item.id)}
    >
      <span class="c-action-item__icon"><Icon name={item.icon} /></span>
      <span class="c-action-item__name">{item.name}</span>
      {#if asks(item.id)}
        <span class="c-badge c-badge--accent">Asks</span>
      {:else}
        <span class="c-badge">Off</span>
      {/if}
      <span class="c-action-item__state">{namesFor(item.id)}</span>
    </button>
  {/each}
  <p class="c-actions__note">
    wtm never runs a recipe on its own. An action with recipes asks you each time; one
    without stays exactly as it is.
  </p>
</div>

<div class="c-action" data-od-id="action-detail">
  <div class="c-action__inner">
    <div>
      <div class="c-action__title">
        <span class="c-action__icon"><Icon name={action.icon} /></span>
        <h3 class="c-action__name">{action.name}</h3>
      </div>
      <p class="c-action__when">{action.when}</p>
    </div>

    <div class="o-stack">
      <h4 class="c-section-heading">Offer these recipes</h4>
      <div class="c-offers">
        {#each bound as recipe, i (recipe.id)}
          <div class="c-offer-row">
            <span class="c-offer-row__order">
              <Button
                variant="quiet"
                size="sm"
                icon="sm"
                disabled={i === 0}
                ariaLabel="Offer {recipe.name} earlier"
                onclick={() => move(ids.indexOf(recipe.id), -1)}
              >
                <Icon name="chevron-up" size={12} />
              </Button>
              <Button
                variant="quiet"
                size="sm"
                icon="sm"
                disabled={i === bound.length - 1}
                ariaLabel="Offer {recipe.name} later"
                onclick={() => move(ids.indexOf(recipe.id), 1)}
              >
                <Icon name="chevron-down" size={12} />
              </Button>
            </span>
            <span class="c-offer-row__main">
              <span class="c-offer-row__name">{recipe.name}</span>
              <span class="c-offer-row__steps"
                >{recipe.steps.map((s) => s.name).join(' → ')}</span
              >
            </span>
            {#if i === 0}
              <span class="c-badge c-badge--accent" title="Picked when the question opens"
                >Preselected</span
              >
            {:else}
              <span></span>
            {/if}
            <Button
              variant="quiet"
              icon="md"
              ariaLabel="Stop offering {recipe.name}"
              onclick={() =>
                void recipes.setOffers(
                  action.id,
                  ids.filter((id) => id !== recipe.id),
                )}
            >
              <Icon name="close" size={14} />
            </Button>
          </div>
        {:else}
          <p class="c-offers__empty">No recipes. {action.name} won’t ask anything.</p>
        {/each}
        <div class="c-offers__add">
          {#if unbound.length === 0}
            <!-- Said, not shown as a disabled control: with one recipe offered and no other to
                 add, an empty select read as though an action could only ever offer one. -->
            <span class="c-offers__hint">
              {recipes.recipes.length === 0
                ? 'No recipes yet.'
                : `Every recipe you have is offered here. Make another — a review, say — to offer it too.`}
            </span>
            <Button
              variant="neutral"
              size="sm"
              onclick={() => settingsView.show('recipes', NEW_RECIPE)}
            >
              <Icon name="plus" size={12} /> New recipe
            </Button>
          {:else}
            <select
              class="c-select"
              aria-label="Recipe to offer"
              value={toAdd}
              onchange={(e) => (adding = (e.currentTarget as HTMLSelectElement).value)}
            >
              {#each unbound as recipe (recipe.id)}
                <option value={recipe.id}>{recipe.name}</option>
              {/each}
            </select>
            <Button variant="neutral" size="sm" onclick={offer}>
              <Icon name="plus" size={12} /> Offer
            </Button>
          {/if}
        </div>
      </div>
      <p class="c-field__help">
        Offer as many as you like. When the action happens you choose which one runs; the
        first is preselected. A recipe limited to other repositories isn’t offered there.
      </p>
    </div>

    <dl class="c-action__facts">
      <dt>Asked</dt>
      <dd>{action.where}</dd>
      <dt>Variables</dt>
      <dd>
        {#each [...PLACE_VARIABLES, ...action.variables] as name (name)}
          <code class="c-var c-var--static">{`{{${name}}}`}</code>
        {/each}
      </dd>
    </dl>

    {#if bound.length > 0}
      <div class="c-action__preview">
        <p class="c-action__preview-label">What you’ll see</p>
        <RecipeOffer
          {action}
          projectId={workspace.activeProjectId ?? ''}
          worktreeId=""
          {place}
          offered={bound}
          preview
          variant={action.id === 'project.added' ? 'toast' : 'card'}
        />
      </div>
    {/if}
  </div>
</div>
