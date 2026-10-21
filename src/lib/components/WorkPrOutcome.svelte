<script lang="ts">
  /**
   * The outcome sheet for a teammate's pull request: what to do with the review once it has read
   * a head. The D1 design's "Teammate PR · choose an outcome".
   *
   * # One live choice, two drawn
   *
   * Keep private notes works: the notes and the review's findings are kept in wtm, pinned to the
   * head the review read, and nothing is posted anywhere. Publish a review and Fix it locally are
   * drawn as the design has them, disabled and saying they come next, because both write to GitHub
   * and every such write waits for the outbox and its approvals (M1b). They are here so the choice
   * reads as the design's, not as a lone button, and so you can see now that wtm will never push to
   * a fork whose author turned maintainer edits off: that destination shows its real availability.
   *
   * # Pinned to the head reviewed
   *
   * The header says which head the review read and which the pull request is at now. When they
   * differ the notes are still about the head reviewed, and the sheet says so rather than hiding
   * the choice: deciding to keep notes on what you read is a fair thing to do.
   */
  import { untrack } from 'svelte';

  import type { PullRequest, ReviewRunView } from '../ipc/types';
  import {
    accessLine,
    currentHead,
    currentOutcome,
    endsLine,
    PUSH_WORDS,
    short,
  } from '../pr-review';
  import { headLabel } from '../work';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';

  const {
    view,
    read,
    busy,
    error,
    onclose,
    onkeep,
  }: {
    view: ReviewRunView;
    /** The window's newest reading of the pull request, which may be newer than the run's. */
    read: PullRequest | null;
    busy: boolean;
    error: string | null;
    onclose: () => void;
    /** Keep the notes. Resolves true when they were kept. */
    onkeep: (notes: string) => Promise<boolean>;
  } = $props();

  const review = $derived(view.pullRequest);
  const pr = $derived(read ?? review?.mirror?.pullRequest ?? null);
  const reviewed = $derived(review?.reviewedHead ?? null);
  const now = $derived(review ? currentHead(review, read) : null);
  const moved = $derived(reviewed !== null && now !== null && reviewed !== now);
  const push = $derived(review?.pushToBranch ?? null);
  const open = $derived(view.findings.filter((f) => f.dismissed === null).length);
  const earlier = $derived(review ? currentOutcome(review) : null);

  // The last notes kept, as a start: keeping again after a new head is usually an edit of them.
  // Read once, when the sheet opens; what you type is yours from then on.
  let notes = $state(untrack(() => earlier?.notes ?? ''));

  async function keep(event: Event): Promise<void> {
    event.preventDefault();
    if (busy) return;
    if (await onkeep(notes)) onclose();
  }
</script>

<Dialog
  title={`Finish your review of #${review?.number ?? ''}`}
  {onclose}
  onsubmit={keep}
  closeDisabled={busy}
  wide
>
  {#snippet body()}
    <div class="c-pr-outcome">
      {#if pr && review}
        <p class="c-pr-outcome__meta">
          <span class="u-mono">{endsLine(pr)}</span>
          {#if reviewed}<span>Reviewed head <code>{short(reviewed)}</code></span>{/if}
          {#if now}
            <span class:c-status--ok={!moved} class:c-status--warn={moved}>
              Current head <code>{short(now)}</code>{moved ? ' · new commits' : ' ✓'}
            </span>
          {/if}
          <span>{accessLine(pr, push)}</span>
        </p>
      {/if}

      <div class="c-pr-outcome__options" role="radiogroup" aria-label="Outcome">
        <div class="c-pr-outcome__option is-selected">
          <label class="c-pr-outcome__choice">
            <input type="radio" name="pr-outcome" checked />
            <span class="c-pr-outcome__text">
              <span class="c-pr-outcome__title">Keep private notes</span>
              <span class="c-pr-outcome__desc"
                >Findings stay in wtm. Nothing is posted to GitHub or Jira.</span
              >
            </span>
          </label>
          <label class="c-pr-outcome__field">
            <span class="c-pr-outcome__label">Notes for yourself</span>
            <textarea
              class="c-textarea"
              rows="4"
              bind:value={notes}
              disabled={busy}
              placeholder="What to ask, what you would check by hand, what you decided"
            ></textarea>
          </label>
          <p class="c-pr-outcome__note">
            {open === 1 ? '1 finding stays' : `${open} findings stay`} beside the notes{reviewed
              ? `, pinned to ${short(reviewed)}`
              : ''}.{moved
              ? ' The pull request has moved on since; Re-review changes first if you want notes on the new head.'
              : ''}
          </p>
        </div>

        <div class="c-pr-outcome__option is-disabled">
          <label class="c-pr-outcome__choice">
            <input type="radio" name="pr-outcome" disabled />
            <span class="c-pr-outcome__text">
              <span class="c-pr-outcome__title"
                >Publish a review <span class="c-badge">Coming next</span></span
              >
              <span class="c-pr-outcome__desc"
                >You choose exactly what is posted, read the inline comments and the summary
                as they will appear, and approve them. Pinned to the head reviewed; a moved
                head asks again.</span
              >
            </span>
          </label>
        </div>

        <div class="c-pr-outcome__option is-disabled">
          <label class="c-pr-outcome__choice">
            <input type="radio" name="pr-outcome" disabled />
            <span class="c-pr-outcome__text">
              <span class="c-pr-outcome__title"
                >Fix it locally, then choose where it goes <span class="c-badge"
                  >Coming next</span
                ></span
              >
              <span class="c-pr-outcome__desc"
                >A writer fixes the selected findings in the review worktree, and you read
                the diff before anything leaves this Mac. Then one of:</span
              >
            </span>
          </label>
          {#if pr}
            <ul class="o-plain-list c-pr-outcome__destinations" aria-label="Destinations">
              <li class="c-pr-outcome__destination">
                Follow-up pull request into <code>{headLabel(pr)}</code> from a new branch
              </li>
              <li class="c-pr-outcome__destination">
                Suggestion comments on #{pr.number} the author can apply
              </li>
              <li
                class="c-pr-outcome__destination"
                class:is-unavailable={push !== null && push !== 'allowed'}
              >
                <span class="c-pr-outcome__target"
                  >Push to <code>{headLabel(pr)}</code></span
                >
                {#if push}<span class="c-pr-outcome__why">{PUSH_WORDS[push]}</span>{/if}
              </li>
            </ul>
          {/if}
        </div>
      </div>
      {#if error}
        <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
      {/if}
    </div>
  {/snippet}
  {#snippet footer()}
    <p class="c-pr-outcome__summary">
      Kept in wtm only{reviewed ? `, pinned to ${short(reviewed)}` : ''}. Nothing is sent
      anywhere.
    </p>
    <Button onclick={onclose} disabled={busy}>Cancel</Button>
    <Button variant="accent" type="submit" disabled={busy}>
      {busy ? 'Keeping…' : 'Keep private notes'}
    </Button>
  {/snippet}
</Dialog>
