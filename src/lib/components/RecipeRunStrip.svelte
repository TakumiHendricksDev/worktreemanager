<script lang="ts">
  /**
   * The recipe running in this worktree, on one line above its panes.
   *
   * Where the agent rail sits, and one line for the rail's reason: every row this band gains is a
   * row the panes lose. Each step is a chip that brings its pane forward; the pane itself is where
   * the work is read, approved and talked to.
   *
   * A finished run stays until dismissed, so a run that failed at step two while nobody was looking
   * still says so when someone comes back.
   */
  import type { RecipeRunStep } from '../ipc/types';
  import { recipes } from '../state/recipes.svelte';
  import { sessions } from '../state/sessions.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const { worktreeId }: { worktreeId: string } = $props();

  const run = $derived(recipes.runIn(worktreeId));

  /** A clock for the elapsed time, ticking only while something is running. */
  let now = $state(Date.now());
  $effect(() => {
    if (run?.status !== 'running') return;
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  const current = $derived(run?.steps.findIndex((s) => s.status === 'running') ?? -1);
  const failed = $derived(run?.steps.find((s) => s.status === 'failed') ?? null);

  function elapsed(from: number, to: number): string {
    const seconds = Math.max(0, Math.round((to - from) / 1000));
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    if (m >= 60) return `${Math.floor(m / 60)}h ${m % 60}m`;
    return m > 0 ? `${m}m ${String(s).padStart(2, '0')}s` : `${s}s`;
  }

  const summary = $derived.by(() => {
    if (!run) return '';
    const took = elapsed(run.startedMs, run.endedMs ?? now);
    switch (run.status) {
      case 'running':
        return `step ${current + 1} of ${run.steps.length} · ${took}`;
      case 'done':
        return `finished in ${took}`;
      case 'stopped':
        return `stopped by you after ${took} · panes stay open`;
      case 'failed':
        return failed
          ? `${failed.name} failed: ${failed.error ?? 'no reason given'}`
          : 'failed';
    }
  });

  const WORD: Record<RecipeRunStep['status'], string> = {
    pending: '',
    running: 'working',
    done: '',
    failed: 'failed',
    skipped: 'skipped',
    stopped: 'stopped',
  };

  function show(step: RecipeRunStep) {
    const pane = sessions.paneBySession(step.session);
    if (pane) sessions.focus(worktreeId, pane.id);
  }

  function detail(step: RecipeRunStep): string {
    const parts = [step.name, step.agent, step.model, step.mode]
      .filter(Boolean)
      .join(' · ');
    return step.error ? `${parts} — ${step.error}` : parts;
  }
</script>

{#if run}
  <nav class="c-run-strip" aria-label="Recipe run" data-od-id="run-strip">
    <span class="c-run-strip__heading">Recipe</span>
    <span class="c-run-strip__name">{run.recipeName}</span>
    <div class="c-run-strip__steps">
      {#each run.steps as step, i (i)}
        {#if i > 0}<span class="c-run-strip__sep" aria-hidden="true">›</span>{/if}
        <button
          type="button"
          class="c-run-strip__step"
          class:is-current={step.status === 'running'}
          class:is-pending={step.status === 'pending' || step.status === 'skipped'}
          class:is-failed={step.status === 'failed'}
          title={detail(step)}
          disabled={step.session === null}
          onclick={() => show(step)}
        >
          {#if step.status === 'done'}
            <span class="c-run-strip__done"><Icon name="check" size={12} /></span>
          {:else if step.status === 'running'}
            <SessionDot status="working" />
          {:else if step.status === 'failed'}
            <Icon name="warn" size={12} />
          {/if}
          {step.name}
          {#if WORD[step.status]}<span class="c-run-strip__word">{WORD[step.status]}</span
            >{/if}
        </button>
      {/each}
    </div>
    {#if run.missing.length > 0}
      <span
        class="c-run-strip__missing"
        title="These were sent empty. The action that offered this recipe doesn't have them."
      >
        no value for {run.missing.map((m) => `{{${m}}}`).join(', ')}
      </span>
    {/if}
    <span
      class="c-run-strip__time"
      class:is-failed={run.status === 'failed'}
      title={summary}>{summary}</span
    >
    {#if run.status === 'running'}
      <Button
        variant="quiet"
        size="sm"
        title="Interrupt the current step and skip the rest"
        onclick={() => void recipes.stop(run.id)}
      >
        <Icon name="stop" size={12} /> Stop recipe
      </Button>
    {:else}
      <Button variant="quiet" size="sm" onclick={() => void recipes.dismiss(run.id)}>
        <Icon name="close" size={12} /> Dismiss
      </Button>
    {/if}
  </nav>
{/if}
