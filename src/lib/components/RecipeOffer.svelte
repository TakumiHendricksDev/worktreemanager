<script lang="ts">
  /**
   * "Run a recipe in feature-x?" — the question an action asks when it has recipes bound.
   *
   * It never runs anything by itself: the first recipe is preselected, and nothing starts until Run
   * is pressed. An action with nothing bound renders nothing at all, so the moment looks exactly as
   * it did before recipes existed — which is the promise the Actions settings make.
   *
   * `preview` draws the same question inert, for Settings to show what a binding will look like.
   */
  import { commands } from '../ipc/commands';
  import {
    errorMessage,
    type Recipe,
    type RecipePreviewStep,
    type RecipeRun,
  } from '../ipc/types';
  import { agentMark, type RecipeAction } from '../recipes';
  import { recipes } from '../state/recipes.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    action,
    projectId,
    worktreeId,
    place,
    vars = {},
    variant = 'card',
    preview = false,
    offered,
    onstarted,
    ondismiss,
  }: {
    action: RecipeAction;
    projectId: string;
    worktreeId: string;
    /** What the question calls the place: a worktree's directory name, or a sentence. */
    place: string;
    vars?: Record<string, string>;
    variant?: 'card' | 'toast';
    preview?: boolean;
    /** The recipes to list; by default what the action offers in this repository. */
    offered?: Recipe[];
    onstarted?: (run: RecipeRun) => void;
    ondismiss?: () => void;
  } = $props();

  const choices = $derived(offered ?? recipes.offeredFor(action.id, projectId));
  let picked = $state<string | null>(null);
  const pick = $derived(choices.find((r) => r.id === picked) ?? choices[0] ?? null);

  let starting = $state(false);
  let showing = $state(false);
  let sent = $state<RecipePreviewStep[] | null>(null);
  let sentError = $state<string | null>(null);

  $effect(() => {
    if (!showing || !pick || preview) return;
    const id = pick.id;
    sent = null;
    sentError = null;
    commands
      .previewRecipe(id, projectId, worktreeId, vars)
      .then((steps) => {
        if (pick?.id === id) sent = steps;
      })
      .catch((e) => (sentError = errorMessage(e)));
  });

  async function run() {
    if (!pick || starting || preview) return;
    starting = true;
    try {
      const started = await recipes.start({
        recipeId: pick.id,
        projectId,
        worktreeId,
        action: action.id,
        vars,
      });
      onstarted?.(started);
    } catch {
      // `recipes.start` has already said why, in a notice; the question stays so it can be retried.
    } finally {
      starting = false;
    }
  }
</script>

{#if choices.length > 0}
  <section
    class="c-offer"
    class:c-offer--toast={variant === 'toast'}
    aria-label="Run a recipe"
    data-od-id="recipe-offer"
  >
    <div class="c-offer__head">
      <span class="c-offer__icon"><Icon name="recipe" /></span>
      <div class="c-offer__titles">
        <p class="c-offer__question">Run a recipe in <code>{place}</code>?</p>
        <p class="c-offer__sub">
          {action.name} offers {choices.length === 1 ? 'this' : 'these'}. Nothing runs
          unless you pick one.
        </p>
      </div>
    </div>

    <div class="c-offer__choices" role="radiogroup" aria-label="Recipes">
      {#each choices as recipe (recipe.id)}
        <label class="c-offer__choice" class:is-selected={pick?.id === recipe.id}>
          <input
            type="radio"
            name="offer-{action.id}-{worktreeId}"
            checked={pick?.id === recipe.id}
            onchange={() => (picked = recipe.id)}
          />
          <span class="c-offer__name">{recipe.name}</span>
          <span class="c-offer__steps">
            {#each recipe.steps as step, i (i)}
              {#if i > 0}<span class="c-offer__sep" aria-hidden="true">→</span>{/if}
              <span class="c-agent-mark c-agent-mark--{step.agent}" aria-hidden="true"
                >{agentMark(step.agent)}</span
              >
              {step.name}
            {/each}
          </span>
        </label>
      {/each}
    </div>

    {#if showing}
      <div class="c-offer__sent">
        {#if sentError}
          <p class="c-status--danger">{sentError}</p>
        {:else if sent === null}
          <p class="c-status--muted">Filling in the variables…</p>
        {:else}
          {#each sent as step, i (i)}
            <div class="c-offer__line">
              <span class="c-offer__number">{i + 1}</span>
              <div class="c-offer__text">
                <code>{step.text}</code>
                {#if step.getsPrevious}
                  <span class="c-offer__more">+ step {i}’s reply</span>
                {/if}
                {#each step.missing as name (name)}
                  <span class="c-status--warn"
                    >No value for {`{{${name}}}`} here; sent empty.</span
                  >
                {/each}
              </div>
            </div>
          {/each}
        {/if}
      </div>
    {/if}

    <div class="c-offer__foot">
      <Button variant="accent" disabled={!pick || starting} onclick={() => void run()}>
        <Icon name="play" size={12} />
        {starting ? 'Starting…' : `Run ${pick?.name ?? ''}`}
      </Button>
      <Button variant="neutral" onclick={() => ondismiss?.()}>Not now</Button>
      <span class="c-offer__spacer"></span>
      {#if !preview && variant === 'card'}
        <Button variant="quiet" size="sm" onclick={() => (showing = !showing)}>
          {showing ? 'Hide' : 'Show'} what it will send
        </Button>
      {/if}
    </div>
  </section>
{/if}
