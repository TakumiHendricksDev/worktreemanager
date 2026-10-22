<script lang="ts">
  /**
   * One publish of a review, as far as it got: the D1 PublishPartial board.
   *
   * # Every write is a step with its own state
   *
   * A review posts as two writes — the inline comments on a pending review, then the submission
   * with the summary and the state — and the card lists both with what happened to each, so a
   * partial publish says exactly which half reached GitHub. The words and which buttons appear are
   * `publish.ts`'s; this lays them out.
   *
   * # What it offers is what can settle it
   *
   * A failure offers Retry, which reads GitHub before sending anything. A pending review in the
   * way offers adding the comments to it, opening it on GitHub, or leaving it as it is. A moved
   * head offers Re-review the changes, or publishing against the reviewed commit anyway, behind a
   * second click that says what that means. While GitHub is being checked after a relaunch, it
   * offers nothing: the check decides whether there is anything left to send.
   *
   * Shown in the Reviews tab under the outcome, and in the focused Inbox for an item about it.
   */
  import type { PublicationView } from '../ipc/types';
  import { short } from '../pr-review';
  import {
    approvalLine,
    eyebrow,
    operationDetail,
    operationTitle,
    operationTone,
    PUBLICATION_TONE,
    publicationActions,
    publicationTitle,
    retryNote,
  } from '../publish';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    view,
    head,
    busy,
    error,
    onretry,
    onadd,
    onleave,
    onrereview,
    onagainst,
    onopen,
  }: {
    view: PublicationView;
    /** The pull request's head by the window's newest reading, when there is one. */
    head: string | null;
    busy: boolean;
    error: string | null;
    onretry: () => void;
    onadd: () => void;
    onleave: () => void;
    /** Re-review the changes the moved head brought. `null` where there is no review to run. */
    onrereview: (() => void) | null;
    /** Approve again against the reviewed commit, with the head named. */
    onagainst: (head: string) => void;
    onopen: (url: string) => void;
  } = $props();

  const tone = $derived(PUBLICATION_TONE[view.state]);
  const actions = $derived(publicationActions(view));
  const checking = $derived(view.state === 'checking');
  const note = $derived(retryNote(view));
  /** The second click "Publish against … anyway" needs. */
  let confirming = $state(false);
</script>

<section
  id="work-publication-{view.approval.id}"
  tabindex="-1"
  class="c-work-state c-publication"
  class:c-work-state--info={tone === 'info'}
  class:c-work-state--warn={tone === 'warn'}
  class:c-work-state--danger={tone === 'danger'}
  class:c-work-state--ok={tone === 'ok'}
  aria-labelledby="publication-{view.approval.id}"
>
  <span class="c-work-state__eyebrow">{eyebrow(view)}</span>
  <h3 class="c-work-state__title" id="publication-{view.approval.id}">
    {publicationTitle(view)}
  </h3>
  <p class="c-work-state__note">{approvalLine(view, head)}</p>

  <ol class="o-plain-list c-publication__steps">
    {#each view.operations as op (op.id)}
      <li class="c-publication__step c-publication__step--{operationTone(op)}">
        <span class="c-publication__mark" aria-hidden="true">
          {#if op.state === 'done'}
            <Icon name="check" size={14} />
          {:else if op.state === 'failed'}
            <Icon name="close" size={14} />
          {:else if op.state === 'needs_attention'}
            <Icon name="warn" size={14} />
          {:else if op.state === 'executing'}
            <Icon name="restart" size={14} />
          {:else}
            <span class="c-publication__dot"></span>
          {/if}
        </span>
        <span class="c-publication__text">
          <span class="c-publication__what">{operationTitle(op, checking)}</span>
          <span class="c-publication__detail">
            <CodeText text={operationDetail(op, view.review.number)} />
            {#if op.state === 'done' && op.result?.url}
              ·
              <Button variant="link" size="sm" onclick={() => onopen(op.result?.url ?? '')}
                >View on GitHub</Button
              >
            {/if}
          </span>
        </span>
      </li>
    {/each}
  </ol>

  {#if checking}
    <p class="c-publication__info" role="status">
      Checking GitHub for what this publish posted before wtm stopped… If it is there, the
      step is marked done; if not, it waits for your Retry. Nothing is sent on its own.
    </p>
  {:else if actions.pending}
    <p class="c-publication__info">
      Add your comments to it and it is submitted with your summary{actions.pending
        .comments > 0
        ? `, along with the ${actions.pending.comments === 1 ? 'comment' : `${actions.pending.comments} comments`} it already holds`
        : ''}. Or open it on GitHub to submit or discard it yourself, then Retry. wtm will
      not start a second one.
    </p>
  {:else if actions.moved}
    <p class="c-publication__info">
      Your approval covered #{view.review.number} at {short(actions.moved.approved)}.
      Re-review the changes to review {short(actions.moved.current)}, or publish what you
      approved against
      {short(view.review.commitId)}: GitHub marks a comment whose line changed since as
      outdated.
    </p>
  {:else if note}
    <p class="c-publication__info">{note}</p>
  {/if}

  {#if error}
    <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
  {/if}

  {#if actions.pending}
    <div class="c-work-state__actions">
      <Button variant="accent" size="sm" disabled={busy} onclick={onadd}
        >Add to my pending review</Button
      >
      <Button size="sm" disabled={busy} onclick={() => onopen(actions.pending?.url ?? '')}
        >Open pending review on GitHub</Button
      >
      <Button size="sm" disabled={busy} onclick={onretry}>Retry</Button>
      <span class="c-publication__gap"></span>
      <Button variant="quiet" size="sm" disabled={busy} onclick={onleave}
        >Leave as is</Button
      >
    </div>
  {:else if actions.moved}
    <div class="c-work-state__actions">
      {#if confirming}
        <Button
          variant="accent"
          size="sm"
          disabled={busy}
          onclick={() => {
            if (actions.moved) onagainst(actions.moved.current);
            confirming = false;
          }}>Publish against {short(view.review.commitId)}</Button
        >
        <Button size="sm" disabled={busy} onclick={() => (confirming = false)}>Back</Button>
      {:else}
        {#if onrereview}
          <Button variant="accent" size="sm" disabled={busy} onclick={onrereview}
            >Re-review the changes</Button
          >
        {/if}
        {#if actions.against}
          <Button size="sm" disabled={busy} onclick={() => (confirming = true)}
            >Publish against {short(view.review.commitId)} anyway…</Button
          >
        {/if}
        <Button variant="quiet" size="sm" disabled={busy} onclick={onleave}>Cancel</Button>
      {/if}
    </div>
  {:else if actions.retry || actions.leave}
    <div class="c-work-state__actions">
      {#if actions.retry}
        <Button variant="accent" size="sm" disabled={busy} onclick={onretry}>Retry</Button>
      {/if}
      {#if actions.leave}
        <Button variant="quiet" size="sm" disabled={busy} onclick={onleave}
          >Leave as is</Button
        >
      {/if}
    </div>
  {:else if view.state === 'published' && view.url}
    <div class="c-work-state__actions">
      <Button size="sm" onclick={() => onopen(view.url ?? '')}>
        <Icon name="external" size={14} /> View on GitHub
      </Button>
    </div>
  {/if}
</section>
