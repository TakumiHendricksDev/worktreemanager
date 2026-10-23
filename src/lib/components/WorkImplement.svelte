<script lang="ts">
  /**
   * An issue's Implement tab: where the writer is, what it committed, what the checks and the
   * review said, each fix round, and what was left when it handed over.
   *
   * A status, not the handoff panel: the handoff — acceptance-criteria coverage, the diff, preview
   * controls — is a later item. What is here is what the run already knows, read from Rust: the
   * rounds and commits the app made, the checks the app ran, the review's findings, the questions
   * the run asked, and why the loop stopped.
   *
   * # In the order it happens
   *
   * 1. **The writer**, its plan revision, the files the plan lets it change, and whether it still
   *    holds the worktree. Start implementing and Resume are Rust's to allow (`canStart`,
   *    `canResume`); a refused click says why and changes nothing.
   * 2. **Rounds**: the plan's, then each fix round, with the commits the app made in it.
   * 3. **Checks**: each configured check's newest record, as the app ran it. Never shown as a pass
   *    when it did not run.
   * 4. **Questions** about work outside the plan, answered in the Inbox.
   * 5. **The hand-over**, once ready to test: why the loop stopped and what it left, with why.
   *    Reading it settles its inbox item, as reading a finished review does.
   */
  import { untrack } from 'svelte';

  import {
    CHECK_TONE,
    CHECK_WORDS,
    emptyRoundWords,
    implementRunWords,
    latestChecks,
    leftWords,
    roundWords,
    shortSha,
    stopWords,
  } from '../implement';
  import { implement } from '../state/implement.svelte';
  import { inbox } from '../state/inbox.svelte';
  import { work } from '../state/work.svelte';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';

  const {
    projectId,
    issueKey,
    visible,
  }: {
    projectId: string;
    issueKey: string;
    visible: boolean;
  } = $props();

  // Read when shown, untracked: `load` writes the store it would otherwise be tracking.
  $effect(() => {
    const project = projectId;
    const key = issueKey;
    if (visible) untrack(() => void implement.load(project, key));
  });

  const view = $derived(implement.views[issueKey] ?? null);

  // A hand-over on screen has been read, as a ready review in the Reviews tab is: its item settles.
  $effect(() => {
    const flow =
      visible && view?.flow.state.phase === 'ready_to_test' ? view.flow.id : null;
    if (flow === null) return;
    untrack(() => inbox.seen(flow));
    return () => untrack(() => inbox.seen(null));
  });
  const busy = $derived(implement.busy[issueKey] ?? false);
  const error = $derived(implement.errors[issueKey] ?? null);
  const implementation = $derived(view?.implementation ?? null);
  const checks = $derived(view ? latestChecks(view.verifying, view.checks) : []);
  const progress = $derived(view?.sessions.at(-1)?.progress ?? null);
</script>

<div class="c-implement">
  {#if error}
    <p class="c-implement__error c-status--danger" role="alert">
      <CodeText text={error} />
    </p>
  {/if}

  {#if !view}
    <div class="c-placeholder">
      <p>
        {issueKey} has no run yet. Start work gives it a worktree and a plan; implementing follows
        the plan you approve.
      </p>
    </div>
  {:else}
    <header class="c-implement__head">
      <h2 class="c-implement__title" data-state={view.flow.state.phase}>
        {implementRunWords(view)}
      </h2>
      <span class="c-implement__actions">
        {#if view.canStart}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            title="Run the writer in the issue's worktree, held to the approved plan"
            onclick={() => void implement.start(projectId, issueKey, view.flow.id)}
            >Start implementing</Button
          >
        {/if}
        {#if view.canResume}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            title="Carry the implementation on where it stopped, in the writer's same conversation"
            onclick={() => void implement.resume(projectId, issueKey, view.flow.id)}
            >Resume</Button
          >
        {/if}
      </span>
    </header>

    {#if !implementation}
      <p class="c-implement__note">
        {#if view.implementer}
          `{view.implementer}` implements this issue once its plan, and a design when the
          plan needs one, is approved.
        {:else}
          This project names no implementer. Add <span class="u-mono"
            >[workflow.implement]</span
          > with a writer role and a review recipe to your own config.
        {/if}
      </p>
    {:else}
      {#if progress && view.running}
        <p class="c-implement__progress"><CodeText text={progress} /></p>
      {/if}

      {#if implementation.outcome === 'ready' && implementation.stopped}
        <section class="c-implement__handover" aria-labelledby="implement-handover">
          <h3 class="c-implement__label" id="implement-handover">Ready to test</h3>
          <p>{stopWords(implementation.stopped)}</p>
          {#if implementation.remaining.length > 0}
            <ul class="o-plain-list c-implement__left">
              {#each implementation.remaining as left (left.finding)}
                <li>
                  <strong>F{left.finding}</strong>
                  <CodeText text={left.title} />
                  {#if left.location}
                    <span class="u-mono c-implement__place"
                      >{left.location.path}:{left.location.startLine}</span
                    >
                  {/if}
                  <span class="c-implement__why">— {leftWords(left.because)}</span>
                </li>
              {/each}
            </ul>
          {/if}
          <p class="c-implement__note">
            Nothing has left this Mac. The worktree is yours again; the handoff panel comes
            with a later version of wtm.
          </p>
        </section>
      {/if}

      <dl class="o-facts o-facts--wide">
        <dt>Writer</dt>
        <dd>
          <span class="u-mono">{implementation.role}</span> · plan revision {implementation.planRevision}
          · {view.lease ? 'holds the worktree' : 'has let the worktree go'}
        </dd>
        <dt>May change</dt>
        <dd>
          <ul class="o-plain-list c-implement__scope">
            {#each implementation.scope.areas as area (area)}
              <li class="u-mono">{area}</li>
            {/each}
            {#each implementation.scope.files as file (file)}
              <li class="u-mono">{file}</li>
            {/each}
          </ul>
        </dd>
        <dt>Fix loop</dt>
        <dd>
          at most {implementation.bounds.rounds} round{implementation.bounds.rounds === 1
            ? ''
            : 's'}, {implementation.bounds.attempts} attempt{implementation.bounds
            .attempts === 1
            ? ''
            : 's'} per problem
        </dd>
        <dt>Review</dt>
        <dd>
          <span class="u-mono">{implementation.recipe}</span>
          {#if view.review}
            · {view.review.running
              ? 'reading now'
              : `${view.review.standing} finding${view.review.standing === 1 ? '' : 's'} standing`}
            <Button size="sm" variant="link" onclick={() => (work.tab = 'reviews')}
              >Open in Reviews</Button
            >
          {:else}
            · not run yet
          {/if}
        </dd>
      </dl>

      <section aria-labelledby="implement-rounds">
        <h3 class="c-implement__label" id="implement-rounds">Rounds</h3>
        <ol class="o-plain-list c-implement__rounds">
          {#each view.rounds as round (round.round)}
            <li class="c-implement__round" data-kind={round.kind}>
              <p class="c-implement__round-head">
                <strong>Round {round.round}</strong> · {roundWords(round)}
                {#if round.endedAt === null}
                  <span class="c-status--info">· under way</span>
                {/if}
              </p>
              <ul class="o-plain-list c-implement__commits">
                {#each view.checkpoints.filter((c) => c.round === round.round) as commit (commit.commit)}
                  <li>
                    <span class="u-mono">{shortSha(commit.commit)}</span>
                    <CodeText text={commit.message.split('\n')[0] ?? ''} />
                    <span class="c-implement__why"
                      >{commit.paths.length} file{commit.paths.length === 1 ? '' : 's'} · {commit.by ===
                      'writer'
                        ? 'its checkpoint'
                        : 'what it left'}</span
                    >
                  </li>
                {:else}
                  <li class="c-implement__why">{emptyRoundWords(round, view.rounds)}</li>
                {/each}
              </ul>
            </li>
          {:else}
            <li class="c-implement__why">The writer has not started.</li>
          {/each}
        </ol>
      </section>

      <section aria-labelledby="implement-checks">
        <h3 class="c-implement__label" id="implement-checks">Checks</h3>
        {#if checks.length === 0}
          <p class="c-implement__note">No checks are configured for implementing.</p>
        {:else}
          <ul class="o-plain-list c-implement__checks">
            {#each checks as check (check.check)}
              <li class="c-implement__check" data-status={check.run?.status ?? 'not_run'}>
                <span class="u-mono">{check.check}</span>
                {#if check.run}
                  <span class="c-status--{CHECK_TONE[check.run.status]}"
                    >{CHECK_WORDS[check.run.status]}</span
                  >
                  <span class="c-implement__why"
                    >{check.runs} run{check.runs === 1 ? '' : 's'}</span
                  >
                {:else}
                  <span class="c-status--muted">not run</span>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      {#if view.questions.length > 0}
        <section aria-labelledby="implement-questions">
          <h3 class="c-implement__label" id="implement-questions">Questions</h3>
          <ul class="o-plain-list c-implement__questions">
            {#each view.questions as asked (asked.link.question)}
              <li>
                <CodeText text={asked.question.text} />
                {#if asked.question.answer}
                  <span class="c-implement__why"
                    >— {asked.question.answer.choice !== null
                      ? asked.question.options[asked.question.answer.choice]
                      : asked.question.answer.text}</span
                  >
                {:else}
                  <span class="c-status--warn">— waiting for you in the Inbox</span>
                {/if}
              </li>
            {/each}
          </ul>
        </section>
      {/if}
    {/if}
  {/if}
</div>
