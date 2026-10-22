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
   *
   * Each reads the issue's start the way its workspace does (`startWork`), so acting here updates
   * both, and Show the issue goes to the workspace's Run section.
   */
  import { untrack } from 'svelte';

  import { KIND_TONE, KIND_WORDS, filedAt } from '../inbox';
  import type { InboxItem } from '../ipc/types';
  import { inbox } from '../state/inbox.svelte';
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
    if (project) untrack(() => void startWork.load(project, key));
  });

  const view = $derived(startWork.views[item.issue] ?? null);
  const busy = $derived(startWork.busy[item.issue] ?? false);
  const error = $derived(startWork.errors[item.issue] ?? inbox.errors[item.id] ?? null);
  const job = $derived(view?.job ?? null);
  const flow = $derived(view?.flow ?? null);
  const move = $derived(
    item.target.kind === 'approval'
      ? (view?.moves.find(
          (m) => item.target.kind === 'approval' && m.approval === item.target.approval,
        ) ?? null)
      : null,
  );
  const pauseOffer = $derived(item.kind === 'approval' && item.target.kind === 'flow');
  const setupRemedy = $derived(
    item.target.kind === 'flow' &&
      (item.kind === 'failure' || item.kind === 'interrupted') &&
      flow !== null &&
      flow.state.end === null &&
      job !== null &&
      (job.state === 'setup_failed' || job.state === 'interrupted'),
  );

  function show(): void {
    work.pickIssue(item.issue);
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
      {#if pauseOffer && flow.state.end === null && flow.state.hold === null}
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
    <Button variant="neutral" size="sm" onclick={show}>Show the issue</Button>
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
