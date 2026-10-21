<script lang="ts">
  /**
   * The Reviews tab's state for one worktree: Run Review when it has not been reviewed, otherwise
   * its newest run's state and each role's line.
   *
   * # A readout, not the Reviews tab
   *
   * What a run is doing and what it needs from you, in the D1 design's words: running, waiting for
   * usage capacity, interrupted, a role failed, reviewed. The findings themselves, grouped and
   * checked for staleness, are the full Reviews tab's (R6); this shows how many each role handed in.
   *
   * Reads `reviewRuns` and nothing else, and every action goes back through it, so the run the
   * engine announced is the run drawn.
   */
  import { untrack } from 'svelte';

  import {
    canResume,
    canRunAgain,
    canStop,
    headline,
    listedSteps,
    resumeWords,
    runsOn,
    skippedLine,
    STEP_TONE,
    stepLine,
  } from '../review-run';
  import { reviewRuns } from '../state/review-runs.svelte';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';

  const {
    projectId,
    worktreeId,
    reviewing,
  }: {
    projectId: string;
    worktreeId: string;
    /** What the review is of, as its title says it: the branch, or a short head. */
    reviewing: string;
  } = $props();

  const run = $derived(reviewRuns.latest[worktreeId]);
  const recipes = $derived(reviewRuns.recipes[projectId] ?? null);
  const busy = $derived(reviewRuns.busy[worktreeId] ?? false);
  const error = $derived(reviewRuns.errors[worktreeId] ?? null);
  const top = $derived(run ? headline(run) : null);

  let chosen = $state<string | null>(null);
  const recipe = $derived(chosen ?? recipes?.[0]?.id ?? null);

  // Read again whenever the worktree shown changes, by id rather than by object: a worktree
  // listing refresh hands over new objects for the same worktrees.
  $effect(() => {
    const project = projectId;
    const worktree = worktreeId;
    untrack(() => {
      void reviewRuns.load(project, worktree);
      void reviewRuns.loadRecipes(project);
    });
  });

  function start(): void {
    if (recipe) void reviewRuns.start(projectId, worktreeId, recipe);
  }
</script>

{#if run && top}
  <section class="c-work-state c-work-state--{top.tone}" aria-labelledby="work-run-title">
    <span class="c-work-state__eyebrow">{top.eyebrow}</span>
    <h2 class="c-work-state__title" id="work-run-title">{top.title}</h2>
    <p class="c-work-state__prose"><CodeText text={top.prose} /></p>

    <ul class="c-run-roles" aria-label="Roles">
      {#each listedSteps(run) as step (step.id)}
        <li class="c-run-roles__row">
          <span
            class="c-run-roles__mark c-run-roles__mark--{STEP_TONE[step.status]}"
            aria-hidden="true"
          ></span>
          <span class="c-run-roles__who">
            {step.role}{step.standsInFor ? ` (for ${step.standsInFor})` : ''} · {runsOn(
              step,
            )}
          </span>
          <span class="c-run-roles__line"><CodeText text={stepLine(run, step)} /></span>
        </li>
      {/each}
    </ul>
    {#if skippedLine(run)}
      <p class="c-work-state__note">{skippedLine(run)}</p>
    {/if}

    <div class="c-work-state__actions">
      {#if canResume(run)}
        <Button
          variant="accent"
          size="sm"
          disabled={busy}
          onclick={() => void reviewRuns.resume(worktreeId, run.flow.id)}
        >
          {resumeWords(run)}
        </Button>
      {/if}
      {#if canStop(run)}
        <Button
          variant="quiet"
          size="sm"
          disabled={busy}
          onclick={() => void reviewRuns.stop(worktreeId, run.flow.id)}>Stop review</Button
        >
      {/if}
      {#if canRunAgain(run) && recipe}
        <Button size="sm" disabled={busy} onclick={start}>Run again</Button>
      {/if}
      <span class="c-work-state__note"
        >Recipe {run.review.recipe} · base {run.review.base}</span
      >
    </div>
    {#if error}
      <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
    {/if}
  </section>
{:else}
  <section class="c-work-state" aria-labelledby="work-review-title">
    <span class="c-work-state__eyebrow">Not reviewed yet</span>
    <h2 class="c-work-state__title" id="work-review-title">Review {reviewing}</h2>
    <p class="c-work-state__prose">
      A review reads a snapshot of this worktree, uncommitted work included, against its
      merge base. It needs no pull request and changes nothing in your tracker.
    </p>
    {#if recipes && recipes.length > 1}
      <label class="c-work-state__field">
        Recipe
        <!-- The value shown is the recipe Run Review would use, the first until one is picked;
             binding `chosen` alone would show a blank select over a default that still applies. -->
        <select
          class="c-select"
          value={recipe}
          onchange={(event) => (chosen = event.currentTarget.value)}
        >
          {#each recipes as choice (choice.id)}
            <option value={choice.id}>{choice.label} · {choice.roles.length} roles</option>
          {/each}
        </select>
      </label>
    {/if}
    <div class="c-work-state__actions">
      <Button variant="accent" disabled={busy || recipe === null} onclick={start}
        >Run Review</Button
      >
      {#if recipes !== null && recipes.length === 0}
        <span class="c-work-state__note"
          >No review recipes yet: add <code>[workflow]</code> roles and a recipe to your own config.</span
        >
      {:else if recipes && recipes[0]}
        <span class="c-work-state__note">
          {(recipes.find((r) => r.id === recipe) ?? recipes[0]).roles
            .map((role) => `${role.id} on ${runsOn(role)}`)
            .join(' · ')}
        </span>
      {/if}
    </div>
    {#if error}
      <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
    {/if}
  </section>
{/if}
