<script lang="ts">
  /**
   * The issue's own pull request, in its Implement tab: the draft each hand-over makes, Mark ready
   * + Needs Review, following it, its feedback, and the Merge sheet.
   *
   * # What is automatic, and what is yours
   *
   * The draft is the plan's one automatic write to GitHub: at each hand-over the task branch is
   * pushed and its draft made or updated, and this section only shows how that went, with Retry.
   * Everything after it is a click of yours, each behind a sheet that reads GitHub and the tracker
   * fresh and says what it will do: Mark ready + Needs Review (two writes, one approval, bound to
   * the head shown), and Merge (one head, one method). Rust decides what may be offered
   * (`canPublish`, `canMerge`); a refusal says why and changes nothing.
   *
   * # Following it, never on a timer
   *
   * Sync reads it now; the window's focus reads it at most once a minute (`App.svelte`). Feedback
   * someone leaves is listed once, and a revision from it is one click.
   */
  import { untrack } from 'svelte';

  import { commands } from '../ipc/commands';
  import type { ImplementView, MergeMethod, MergeSheet, ReadySheet } from '../ipc/types';
  import {
    METHOD_WORDS,
    operationWords,
    readingWords,
    retryable,
    sendTitle,
    shortSha,
  } from '../own-pr';
  import { implement } from '../state/implement.svelte';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';

  const {
    projectId,
    issueKey,
    view,
    visible,
  }: {
    projectId: string;
    issueKey: string;
    view: ImplementView;
    visible: boolean;
  } = $props();

  const flowId = $derived(view.flow.id);
  $effect(() => {
    const flow = flowId;
    const key = issueKey;
    if (!visible) return;
    untrack(() => void implement.loadPull(key, flow));
    return () => untrack(() => implement.forgetPull(key));
  });

  const pull = $derived(implement.pulls[issueKey] ?? null);
  const busy = $derived(implement.busy[issueKey] ?? false);
  const newest = (kind: 'draft' | 'ready' | 'merge') =>
    [...(pull?.sends ?? [])].reverse().find((s) => s.request.kind === kind) ?? null;
  const draft = $derived(newest('draft'));
  const ready = $derived(newest('ready'));
  const merge = $derived(newest('merge'));
  const shown = $derived([draft, ready, merge].filter((s) => s !== null));
  const open = $derived(pull?.feedback.filter((f) => f.revisedIn === null) ?? []);

  let readySheet = $state<ReadySheet | null>(null);
  let mergeSheet = $state<MergeSheet | null>(null);
  let method = $state<MergeMethod>('squash');

  async function openReady(): Promise<void> {
    readySheet = await implement.readySheet(issueKey, flowId);
  }

  async function openMerge(): Promise<void> {
    method = 'squash';
    mergeSheet = await implement.mergeSheet(issueKey, flowId);
  }

  async function approveReady(): Promise<void> {
    if (!readySheet) return;
    const head = readySheet.head;
    readySheet = null;
    await implement.approveReady(projectId, issueKey, flowId, head);
  }

  async function approveMerge(): Promise<void> {
    if (!mergeSheet) return;
    const head = mergeSheet.head;
    mergeSheet = null;
    await implement.approveMerge(projectId, issueKey, flowId, head, method);
  }
</script>

{#if pull}
  <section class="c-own-pr" aria-labelledby="own-pr-title">
    <header class="c-own-pr__head">
      <h3 class="c-implement__label" id="own-pr-title">Pull request</h3>
      <span class="c-own-pr__actions">
        {#if pull.url && pull.pullRequest}
          {@const url = pull.url}
          <Button size="sm" variant="link" onclick={() => void commands.openUrl(url)}
            >#{pull.pullRequest.number} on GitHub</Button
          >
        {/if}
        {#if pull.canSync}
          <Button
            size="sm"
            disabled={busy}
            title="Read its checks, reviews and merge state from GitHub now"
            onclick={() => void implement.syncPull(issueKey, flowId, 'demand')}>Sync</Button
          >
        {/if}
      </span>
    </header>

    {#if pull.unavailable}
      <p class="c-own-pr__note">
        <CodeText text={`Nothing is drafted: ${pull.unavailable}.`} />
      </p>
    {:else}
      <dl class="o-facts o-facts--wide">
        <dt>Branch</dt>
        <dd>
          <span class="u-mono">{pull.branch}</span> →
          <span class="u-mono">{pull.base}</span>
          {#if pull.repo}in <span class="u-mono">{pull.repo}</span>{/if}
        </dd>
        <dt>GitHub</dt>
        <dd>
          {#if pull.reading}
            {readingWords(pull.reading.pullRequest)}
            <span class="c-own-pr__why"
              >· read {new Date(pull.reading.readAt).toLocaleTimeString()}</span
            >
          {:else if pull.pullRequest}
            not read yet: Sync reads it
          {:else}
            no pull request yet: the hand-over drafts it
          {/if}
        </dd>
      </dl>

      {#each shown as send (send.approval.id)}
        <div class="c-own-pr__send" data-state={send.state}>
          <p class="c-own-pr__send-head">
            <strong>{sendTitle(send)}</strong>
            <span class="c-own-pr__why">· {send.state.replace('_', ' ')}</span>
            {#if retryable(send)}
              <Button
                size="sm"
                disabled={busy}
                title="Read first, then send only what is missing"
                onclick={() =>
                  void implement.retryWrite(projectId, issueKey, send.approval.id)}
                >Retry</Button
              >
            {/if}
          </p>
          <ul class="o-plain-list c-own-pr__ops">
            {#each send.operations as op (op.id)}
              <li data-state={op.state}>
                {operationWords(op)}
                {#if op.lastError && op.state !== 'done'}
                  <span class="c-own-pr__why">— <CodeText text={op.lastError} /></span>
                {/if}
                {#if op.attention && op.state === 'needs_attention'}
                  <span class="c-status--warn"
                    >— {op.attention.kind === 'head_moved'
                      ? `the head moved to ${shortSha(op.attention.current)}; open the sheet again`
                      : op.attention.kind === 'status_moved'
                        ? `the issue is in ${op.attention.current} now`
                        : op.attention.kind === 'refused'
                          ? op.attention.message
                          : 'stopped'}</span
                  >
                {/if}
              </li>
            {/each}
          </ul>
        </div>
      {/each}

      {#if pull.feedback.length > 0}
        <section aria-labelledby="own-pr-feedback">
          <h4 class="c-own-pr__heading" id="own-pr-feedback">Feedback</h4>
          <ul class="o-plain-list c-own-pr__feedback">
            {#each pull.feedback as review (review.review)}
              <li class="c-own-pr__review" data-revised={review.revisedIn !== null}>
                <p>
                  <strong>{review.author ?? 'someone'}</strong>
                  {review.state === 'changes_requested' ? 'asked for changes' : 'commented'}
                  {#if review.revisedIn !== null}
                    <span class="c-own-pr__why">· taken into round {review.revisedIn}</span>
                  {/if}
                </p>
                {#if review.body.trim()}
                  <p class="c-own-pr__body"><CodeText text={review.body} /></p>
                {/if}
                {#each review.comments as comment, i (i)}
                  <p class="c-own-pr__body">
                    <span class="u-mono">{comment.path}</span>: <CodeText
                      text={comment.body}
                    />
                  </p>
                {/each}
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      <div class="c-own-pr__row">
        {#if pull.canReviseFeedback && open.length > 0}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            title="Send the feedback to the implementer, in its same conversation"
            onclick={() => void implement.reviseFromFeedback(projectId, issueKey, flowId)}
            >Start a revision from the feedback</Button
          >
        {/if}
        {#if pull.canPublish}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            onclick={() => void openReady()}>Mark ready + Needs Review…</Button
          >
        {/if}
        {#if pull.canMerge}
          <Button size="sm" disabled={busy} onclick={() => void openMerge()}>Merge…</Button>
        {/if}
      </div>
    {/if}
  </section>
{/if}

{#if readySheet}
  {@const sheet = readySheet}
  <Dialog
    title={`Mark ready + Needs Review — ${sheet.key}`}
    onclose={() => (readySheet = null)}
    onsubmit={(event) => {
      event.preventDefault();
      void approveReady();
    }}
  >
    {#snippet body()}
      <ol class="c-own-pr__sheet">
        <li>
          <strong>#{sheet.number}</strong>: {sheet.draft
            ? 'draft → ready'
            : 'already ready'} · head <span class="u-mono">{shortSha(sheet.head)}</span>
          {#if sheet.head === sheet.handedOver}
            <span class="c-status--ok">(the hand-over you tested)</span>
          {:else}
            <span class="c-status--warn"
              >(the worktree is at {shortSha(sheet.handedOver)})</span
            >
          {/if}
        </li>
        <li>
          <strong>{sheet.key}</strong>: {sheet.trackerFrom ?? 'unknown'} → {sheet.trackerTo ??
            'no status mapped'}
        </li>
      </ol>
      <p class="c-own-pr__note">
        One approval of both, in that order. Stale if the head changes; the move stops if
        the issue moved meanwhile. Retry after a partial success sends only what is missing.
      </p>
      {#if sheet.problems.length > 0}
        <ul class="o-plain-list c-own-pr__problems" role="alert">
          {#each sheet.problems as problem (problem)}
            <li><CodeText text={problem} /></li>
          {/each}
        </ul>
      {/if}
    {/snippet}
    {#snippet footer()}
      <Button variant="neutral" onclick={() => (readySheet = null)}>Cancel</Button>
      <Button type="submit" variant="accent" disabled={busy || sheet.problems.length > 0}
        >Approve both</Button
      >
    {/snippet}
  </Dialog>
{/if}

{#if mergeSheet}
  {@const sheet = mergeSheet}
  <Dialog
    title={`Merge pull request #${sheet.number}`}
    onclose={() => (mergeSheet = null)}
    onsubmit={(event) => {
      event.preventDefault();
      void approveMerge();
    }}
  >
    {#snippet body()}
      <dl class="o-facts o-facts--wide">
        <dt>Repository</dt>
        <dd class="u-mono">{sheet.repo}</dd>
        <dt>Branches</dt>
        <dd>
          <span class="u-mono">{sheet.branch}</span> →
          <span class="u-mono">{sheet.base}</span>
        </dd>
        <dt>Head</dt>
        <dd class="u-mono">{shortSha(sheet.head)}</dd>
        <dt>GitHub</dt>
        <dd>{readingWords(sheet.pullRequest)}</dd>
        <dt>Method</dt>
        <dd>
          <select class="c-select" bind:value={method} aria-label="Merge method">
            {#each sheet.methods as m (m)}
              <option value={m}>{METHOD_WORDS[m]}</option>
            {/each}
          </select>
        </dd>
      </dl>
      <p class="c-own-pr__note">
        Only this sheet merges. GitHub is told the head too, and refuses the merge if the
        pull request is no longer at {shortSha(sheet.head)}.
      </p>
      {#if sheet.problems.length > 0}
        <ul class="o-plain-list c-own-pr__problems" role="alert">
          {#each sheet.problems as problem (problem)}
            <li><CodeText text={problem} /></li>
          {/each}
        </ul>
      {/if}
    {/snippet}
    {#snippet footer()}
      <Button variant="neutral" onclick={() => (mergeSheet = null)}>Cancel</Button>
      <Button type="submit" variant="accent" disabled={busy || sheet.problems.length > 0}
        >Merge {shortSha(sheet.head)}</Button
      >
    {/snippet}
  </Dialog>
{/if}
