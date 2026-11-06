<script lang="ts">
  import ExtensionPoint from './ExtensionPoint.svelte';
  import { extensionContext } from '../state/extensions.svelte';
  const publicContext = extensionContext();
  /**
   * A new worktree's record of being made: how it went, its setup output, and what to do next.
   *
   * Shown in the worktree's own view, where an empty worktree asks you to start a session — a
   * worktree just made has no panes yet, so the record and the first session share a screenful
   * instead of living in two places. It stays until dismissed; a worktree that was made cleanly
   * also lets it go once a session opens there, since that is what it was waiting for.
   *
   * A setup that failed is not an error, and this is where that is honoured: the worktree is kept,
   * and the record carries Re-run setup, Remove, the transcript and whatever "Setup failed" offers
   * rather than throwing away what was built.
   */
  import { actionById, type RecipeAction } from '../recipes';
  import {
    creations,
    currentStep,
    madeWorktree,
    type Creation,
  } from '../state/creations.svelte';
  import { worktreeRemoval } from '../state/worktree-removal.svelte';
  import RecipeOffer from './RecipeOffer.svelte';
  import RemoveWorktreeDialog from './RemoveWorktreeDialog.svelte';
  import Terminal from './Terminal.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const { creation }: { creation: Creation } = $props();

  const CREATED = actionById('worktree.created') as RecipeAction;
  const SETUP_FAILED = actionById('worktree.setup_failed') as RecipeAction;

  const worktree = $derived(madeWorktree(creation));
  const failed = $derived(
    creation.phase === 'setup_failed' || creation.phase === 'cancelled',
  );
  const removing = $derived(
    worktree !== null && worktreeRemoval.isRemoving(creation.projectId, worktree.id),
  );
  const step = $derived(currentStep(creation));
  let showOutput = $state(false);
  let confirmingRemove = $state(false);

  /** A failure's transcript is the evidence, so it starts open; a clean one's starts closed. */
  $effect(() => {
    showOutput = failed || creation.rerunning;
  });

  const heading = $derived.by(() => {
    if (creation.rerunning) return 'Running setup again…';
    const outcome = creation.outcome;
    if (outcome?.kind === 'setup_failed')
      return outcome.outcome.kind === 'timed_out' ? 'Setup timed out' : 'Setup failed';
    if (outcome?.kind === 'cancelled') return 'Setup was cancelled';
    return null;
  });

  /** How setup ended, in words, for a record that did not end well: the run's own error first. */
  const ended = $derived.by(() => {
    if (creation.error) return creation.error;
    const outcome = creation.outcome;
    if (outcome?.kind !== 'setup_failed') return null;
    const exit = outcome.outcome;
    switch (exit.kind) {
      case 'failed':
        return `setup exited with code ${exit.code}`;
      case 'signalled':
        return `setup was stopped by signal ${exit.signal}`;
      case 'timed_out':
        return `setup ran out of time after ${Math.round(exit.afterMs / 1000)}s`;
      default:
        return null;
    }
  });

  function took(): string {
    if (creation.finishedMs === null) return '';
    const seconds = Math.max(
      1,
      Math.round((creation.finishedMs - creation.startedMs) / 1000),
    );
    return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  }

  /** The `{{form.*}}` values the worktree was made from, normalized as its fields made them. */
  const formVars = $derived(
    Object.fromEntries(
      Object.entries({ ...creation.values, ...creation.normalized }).map(([key, value]) => [
        `form.${key}`,
        value,
      ]),
    ),
  );
  const failureVars = $derived.by(() => {
    const outcome = creation.outcome;
    const exit = outcome?.kind === 'setup_failed' ? outcome.outcome : null;
    return {
      ...formVars,
      'action.outcome': exit?.kind ?? 'cancelled',
      'action.exit_code': exit?.kind === 'failed' ? String(exit.code) : '',
    };
  });
</script>

<ExtensionPoint target="component.creation-record" context={publicContext()}>
  {#if worktree}
    <section
      class="c-creation-record"
      aria-label="How {creation.dirname} was made"
      data-od-id="creation-record"
    >
      <div class="c-creation-record__head">
        <span
          class="c-creation-record__mark"
          class:c-creation-record__mark--warn={failed && !creation.rerunning}
        >
          <Icon name={failed && !creation.rerunning ? 'warn' : 'check'} size={16} />
        </span>
        <div class="c-creation-record__titles">
          {#if heading}
            <p
              class="c-creation-record__title"
              class:c-status--warn={failed && !creation.rerunning}
            >
              {heading}
            </p>
            <p class="c-creation-record__sub">
              The worktree <code>{creation.dirname}</code> was created and kept{#if ended}{` · ${ended}`}{/if}
            </p>
          {:else}
            <p class="c-creation-record__title">Created <code>{creation.dirname}</code></p>
            <p class="c-creation-record__sub">
              on {worktree.branch ??
                'a detached HEAD'}{#if took()}{` · took ${took()}`}{/if}
            </p>
          {/if}
        </div>
        <Button
          variant="quiet"
          size="sm"
          title="Hide this record; the worktree stays"
          disabled={creation.rerunning}
          onclick={() => creations.dismiss(creation.id)}
        >
          Dismiss
        </Button>
      </div>

      {#if creation.rerunning}
        {#if step}
          <p class="c-creation-record__step">
            <Icon name="chevron-right" size={12} />
            {step.label}
            {#if creation.command}· <code>{creation.command.argv.join(' ')}</code>{/if}
          </p>
        {/if}
      {:else if failed}
        <p class="c-note">
          It has <strong>not</strong> been removed. Setup may already have written an environment
          file, allocated ports or cloned a database volume — deleting the worktree would leak
          those and lose work that is usually one command from fixed.
        </p>
        <div class="o-row">
          <Button
            variant="neutral"
            disabled={removing}
            onclick={() => void creations.rerun(creation.id)}>Re-run setup</Button
          >
          <Button
            variant="danger-outline"
            disabled={removing || confirmingRemove}
            onclick={() => (confirmingRemove = true)}
            >{removing ? 'Removing…' : 'Remove the worktree'}</Button
          >
        </div>
      {/if}

      {#if creation.session}
        <div class="c-creation-record__output">
          <button
            type="button"
            class="c-creation-record__toggle"
            aria-expanded={showOutput}
            onclick={() => (showOutput = !showOutput)}
          >
            <Icon name={showOutput ? 'chevron-down' : 'chevron-right'} size={12} />
            <Icon name="terminal" size={14} />
            Setup output
          </button>
          {#if showOutput}
            <div class="c-creation-record__terminal">
              {#key creation.session}
                <Terminal session={creation.session} />
              {/key}
            </div>
          {/if}
        </div>
      {/if}

      {#if !creation.rerunning && !creation.offerDismissed}
        <RecipeOffer
          action={failed ? SETUP_FAILED : CREATED}
          projectId={creation.projectId}
          worktreeId={worktree.id}
          place={creation.dirname}
          vars={failed ? failureVars : formVars}
          ondismiss={() => creations.dismissOffer(creation.id)}
        />
      {/if}
    </section>

    {#if confirmingRemove}
      <RemoveWorktreeDialog
        projectId={creation.projectId}
        {worktree}
        onclose={() => (confirmingRemove = false)}
      />
    {/if}
  {/if}
</ExtensionPoint>
