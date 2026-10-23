<script lang="ts">
  /**
   * An inbox item about an issue's own run — Start Work's — open, with what settles it.
   *
   * - **"Pause its run?"**, offered when the issue was dragged backward on the board. Pausing holds
   *   the run and touches nothing else; keeping it running is putting the offer away.
   * - **A move that stopped**, because the issue was found in another status than the one it was
   *   moved from: move it anyway, from where it is now, or leave it there.
   * - **A move that failed**: retry, which reads the issue again first, or leave it.
   * - **A worktree's setup that failed, or a start a quit or crash cut off**: retry setup, which runs
   *   the project's setup in the worktree that is there, or use the worktree as it is.
   * - **Its plan**: a plan ready for your decision, or an approval the issue's edit made stale, go
   *   to the Plan tab, where the plan is read whole before it is decided on; a planner that was cut
   *   off or stopped is resumed in its own conversation. A plan-ready item is an approval about the
   *   run, as a pause offer is, and is told apart by the run being held for it at Planned.
   * - **Its design**: a revision ready for your decision, or a design approval its sources made
   *   stale, go to the Design tab, where every revision can be previewed and commented on before
   *   it is decided on; a designer that was cut off or stopped is resumed in its own conversation.
   * - **Its implementation**: ready to test goes to the Implement tab, where what was done and what
   *   was left is listed; an implementation that was cut off, paused at a limit or stopped is
   *   resumed where it stopped, under its lease, in the writer's conversation.
   *
   * Each reads the issue's start the way its workspace does (`startWork`), so acting here updates
   * both, and Show the issue goes to the workspace's Run section.
   */
  import { untrack } from 'svelte';

  import { KIND_TONE, KIND_WORDS, filedAt } from '../inbox';
  import type { InboxItem } from '../ipc/types';
  import { inbox } from '../state/inbox.svelte';
  import { design } from '../state/design.svelte';
  import { implement } from '../state/implement.svelte';
  import { plan } from '../state/plan.svelte';
  import { startWork } from '../state/start-work.svelte';
  import { view as mainView } from '../state/view.svelte';
  import { work } from '../state/work.svelte';
  import { moveWords, raceNote } from '../start-work';
  import { TRACKER_NAMES } from '../tracker';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';

  const { item }: { item: InboxItem & { issue: string } } = $props();

  // Read with the item, untracked: `load` writes the store it reads.
  $effect(() => {
    const project = item.project;
    const key = item.issue;
    if (project)
      untrack(() => {
        void startWork.load(project, key);
        void plan.load(project, key);
        void design.load(project, key);
        void implement.load(project, key);
      });
  });

  const view = $derived(startWork.views[item.issue] ?? null);
  const busy = $derived(startWork.busy[item.issue] ?? false);
  const job = $derived(view?.job ?? null);
  const flow = $derived(view?.flow ?? null);
  const move = $derived(
    item.target.kind === 'approval'
      ? (view?.moves.find(
          (m) => item.target.kind === 'approval' && m.approval === item.target.approval,
        ) ?? null)
      : null,
  );
  const planned = $derived(plan.views[item.issue] ?? null);
  /** The plan waits for your decision: the run is held for it at Planned. */
  const planReady = $derived(
    item.kind === 'approval' &&
      item.target.kind === 'flow' &&
      planned?.flow.state.phase === 'planned' &&
      planned.flow.state.hold?.kind === 'awaiting_approval',
  );
  const designed = $derived(design.views[item.issue] ?? null);
  /** A design revision waits for your decision: the run is held for it at Design ready. */
  const designReady = $derived(
    item.kind === 'approval' &&
      item.target.kind === 'flow' &&
      designed?.flow.state.phase === 'design_ready' &&
      designed.flow.state.hold?.kind === 'awaiting_approval',
  );
  /** The approved design no longer covers what would be built. */
  const designStale = $derived(
    item.target.kind === 'approval' &&
      designed?.standing.kind === 'stale' &&
      designed.standing.approval === item.target.approval,
  );
  /** The designer was cut off or stopped part-way through, and nothing else holds it. */
  const designerStopped = $derived(
    item.target.kind !== 'approval' &&
      (item.kind === 'failure' ||
        item.kind === 'interrupted' ||
        item.kind === 'capacity') &&
      designed !== null &&
      designed.canStart &&
      designed.sessions.length > 0,
  );
  const implemented = $derived(implement.views[item.issue] ?? null);
  /** The implementation handed over: what it did and left is in the Implement tab. */
  const readyToTest = $derived(
    item.kind === 'review_ready' && implemented?.flow.state.phase === 'ready_to_test',
  );
  /** The implementation was cut off, paused at a limit, or stopped, and can carry on. */
  const implementerStopped = $derived(
    item.target.kind !== 'approval' &&
      (item.kind === 'failure' ||
        item.kind === 'interrupted' ||
        item.kind === 'capacity') &&
      implemented !== null &&
      implemented.canResume,
  );
  // The Design tab's refusals are the tab's, Start implementing's among them; this card shows only
  // what its own Resume designer, or Resume, was refused.
  const error = $derived(
    startWork.errors[item.issue] ??
      plan.errors[item.issue] ??
      (designerStopped ? design.errors[item.issue] : undefined) ??
      (implementerStopped ? implement.errors[item.issue] : undefined) ??
      inbox.errors[item.id] ??
      null,
  );
  /** The issue changed after its plan was approved. */
  const planStale = $derived(
    item.target.kind === 'approval' &&
      planned?.standing.kind === 'stale' &&
      planned.standing.approval === item.target.approval,
  );
  // Feedback on the issue's own pull request, or one of its writes that stopped or failed: the
  // Implement tab holds the pull request, its sheets and Retry.
  const aboutPullRequest = $derived(
    implemented !== null &&
      (item.kind === 'feedback' ||
        (item.target.kind === 'approval' &&
          (item.kind === 'failure' || item.kind === 'approval') &&
          move === null &&
          !planStale &&
          !designStale)),
  );
  /** The planner was cut off or stopped part-way through, and nothing else holds it. */
  const plannerStopped = $derived(
    item.target.kind === 'flow' &&
      (item.kind === 'failure' ||
        item.kind === 'interrupted' ||
        item.kind === 'capacity') &&
      planned !== null &&
      planned.flow.state.end === null &&
      planned.flow.state.phase === 'investigating' &&
      !planned.running &&
      planned.flow.state.hold?.kind !== 'awaiting_answer',
  );
  const pauseOffer = $derived(
    item.kind === 'approval' && item.target.kind === 'flow' && !planReady && !designReady,
  );
  const setupRemedy = $derived(
    item.target.kind === 'flow' &&
      (item.kind === 'failure' || item.kind === 'interrupted') &&
      flow !== null &&
      flow.state.end === null &&
      job !== null &&
      (job.state === 'setup_failed' || job.state === 'interrupted'),
  );

  function show(tab: 'overview' | 'plan' | 'design' | 'implement' = 'overview'): void {
    work.pickIssue(item.issue, tab);
    mainView.show('work');
  }
</script>

<article class="c-inbox-card" aria-labelledby="inbox-card-{item.id}">
  <div class="c-inbox-card__meta">
    <span class="c-inbox-card__badge c-inbox-card__badge--{KIND_TONE[item.kind]}"
      >{KIND_WORDS[item.kind]}</span
    >
    <span>{item.issue} · {filedAt(item.createdAt)}</span>
  </div>
  <h2 class="c-inbox-card__title" id="inbox-card-{item.id}">
    <CodeText text={item.title} />
  </h2>
  <p class="c-inbox-card__body"><CodeText text={item.body} /></p>
  {#if move}
    <p class="c-inbox-card__note">{moveWords(move)}. {raceNote(TRACKER_NAMES.jira)}</p>
  {/if}

  <div class="c-inbox-card__actions">
    {#if item.project && flow}
      {@const project = item.project}
      {#if readyToTest}
        <Button variant="accent" size="sm" onclick={() => show('implement')}
          >See what was done</Button
        >
      {:else if aboutPullRequest}
        <Button variant="accent" size="sm" onclick={() => show('implement')}
          >{item.kind === 'feedback' ? 'See the feedback' : 'Open its pull request'}</Button
        >
      {:else if implementerStopped && implemented}
        <Button
          variant="accent"
          size="sm"
          disabled={implement.busy[item.issue] ?? false}
          onclick={() => void implement.resume(project, item.issue, implemented.flow.id)}
          >Resume implementing</Button
        >
      {:else if designReady || designStale}
        <Button variant="accent" size="sm" onclick={() => show('design')}
          >{designStale ? 'Show what changed' : 'Review the design'}</Button
        >
      {:else if designerStopped && designed}
        <Button
          variant="accent"
          size="sm"
          disabled={design.busy[item.issue] ?? false}
          onclick={() => void design.start(project, item.issue, designed.flow.id)}
          >Resume designer</Button
        >
      {:else if planReady || planStale}
        <Button variant="accent" size="sm" onclick={() => show('plan')}
          >{planStale ? 'Show what changed' : 'Read the plan'}</Button
        >
      {:else if plannerStopped && planned}
        <Button
          variant="accent"
          size="sm"
          disabled={plan.busy[item.issue] ?? false}
          onclick={() => void plan.resume(project, item.issue, planned.flow.id)}
          >Resume planner</Button
        >
      {:else if pauseOffer && flow.state.end === null && flow.state.hold === null}
        <Button
          variant="accent"
          size="sm"
          disabled={busy}
          onclick={() => void startWork.pause(project, item.issue, flow.id)}
          >Pause the run</Button
        >
        <Button size="sm" disabled={busy} onclick={() => void inbox.dismiss(item)}
          >Keep it running</Button
        >
      {:else if move && (move.state === 'needs_you' || move.state === 'failed')}
        {#if move.operation.attention?.kind === 'status_moved'}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            onclick={() =>
              void startWork.settleMove(project, item.issue, move.approval, 'anyway')}
            >Move to {move.request.payload.to} anyway</Button
          >
        {:else}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            onclick={() =>
              void startWork.settleMove(project, item.issue, move.approval, 'retry')}
            >Retry</Button
          >
        {/if}
        <Button
          size="sm"
          disabled={busy}
          onclick={() =>
            void startWork.settleMove(project, item.issue, move.approval, 'leave')}
          >Leave it</Button
        >
      {:else if setupRemedy && job}
        <Button
          variant="accent"
          size="sm"
          disabled={busy || view?.working}
          onclick={() => void startWork.resume(project, item.issue, flow.id, false)}
          >{job.state === 'setup_failed' ? 'Retry setup' : 'Resume'}</Button
        >
        <Button
          size="sm"
          disabled={busy || view?.working}
          onclick={() => void startWork.resume(project, item.issue, flow.id, true)}
          >Use as it is</Button
        >
      {/if}
    {/if}
    <Button variant="neutral" size="sm" onclick={() => show()}>Show the issue</Button>
    <span class="c-inbox-card__spacer"></span>
    <Button
      variant="quiet"
      size="sm"
      disabled={busy}
      onclick={() => void inbox.dismiss(item)}>Dismiss</Button
    >
  </div>
  {#if error}
    <p class="c-inbox-card__error" role="alert"><CodeText text={error} /></p>
  {/if}
</article>
