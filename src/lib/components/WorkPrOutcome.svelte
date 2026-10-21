<script lang="ts">
  /**
   * The outcome sheet for a teammate's pull request: what to do with the review once it has read
   * a head. The D1 design's "Teammate PR · choose an outcome".
   *
   * # Two live choices, one drawn
   *
   * Keep private notes keeps the notes and the review's findings in wtm, pinned to the head the
   * review read, and posts nothing. Publish a review posts what you choose, and only that: pick the
   * findings, edit what each comment says, write the summary, choose the review state, then
   * **Preview exactly what posts**, which asks the app to build the review from a fresh read of the
   * pull request and its diff and shows every body as GitHub will receive it, hidden markers
   * included. Approve and publish is open only while that preview is of the draft as it stands: an
   * edit after it, or a finding that changed under it, needs a new preview. Fix it locally is drawn
   * as the design has it, disabled, until the writer lease exists (P3); its push destination
   * already shows its real availability.
   *
   * # Pinned to the head reviewed
   *
   * The header says which head the review read and which the pull request is at now. A review is
   * posted against the commit it read. If the pull request moved, approving is refused with both
   * heads, and the sheet offers what the PublishPartial board does: re-review the changes, or
   * publish against the commit you reviewed anyway — a second click, since GitHub then marks a
   * comment on a changed line as outdated.
   *
   * # Nothing here posts by itself
   *
   * Approving records the approval and hands the publish to the outbox; the sheet closes and the
   * Reviews tab shows its progress (`WorkPublication.svelte`). No agent can reach any of this: it
   * is the window's IPC, never a run tool.
   */
  import { untrack } from 'svelte';

  import type {
    PublicationView,
    PublishPreview,
    PullRequest,
    ReviewDraft,
    ReviewEvent,
    ReviewRunView,
  } from '../ipc/types';
  import { isWtmError } from '../ipc/types';
  import { SEVERITY_WORDS } from '../knowledge';
  import {
    accessLine,
    currentHead,
    currentOutcome,
    endsLine,
    PUSH_WORDS,
    short,
  } from '../pr-review';
  import {
    commentDraft,
    EVENT_WORDS,
    findingPlace,
    FOLD_WORDS,
    pickedByDefault,
    postsLine,
    sameDraft,
  } from '../publish';
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
    onpreview,
    onapprove,
    onrereview,
  }: {
    view: ReviewRunView;
    /** The window's newest reading of the pull request, which may be newer than the run's. */
    read: PullRequest | null;
    busy: boolean;
    error: string | null;
    onclose: () => void;
    /** Keep the notes. Resolves true when they were kept. */
    onkeep: (notes: string) => Promise<boolean>;
    /** Exactly what a draft would post. Rejects with the app's error. */
    onpreview: (draft: ReviewDraft) => Promise<PublishPreview>;
    /** Approve what was previewed, by its hash. Rejects with the app's error, by kind. */
    onapprove: (
      draft: ReviewDraft,
      hash: string,
      againstHead: string | null,
    ) => Promise<PublicationView>;
    /** Re-review the pull request's new head instead. */
    onrereview: (head: string) => void;
  } = $props();

  type Choice = 'private' | 'publish';

  const review = $derived(view.pullRequest);
  const pr = $derived(read ?? review?.mirror?.pullRequest ?? null);
  const reviewed = $derived(review?.reviewedHead ?? null);
  const now = $derived(review ? currentHead(review, read) : null);
  const moved = $derived(reviewed !== null && now !== null && reviewed !== now);
  const push = $derived(review?.pushToBranch ?? null);
  const standing = $derived(view.findings.filter((f) => f.dismissed === null));
  const earlier = $derived(review ? currentOutcome(review) : null);

  // Read once, when the sheet opens; what you type and pick is yours from then on.
  let notes = $state(untrack(() => earlier?.notes ?? ''));
  let choice = $state<Choice>(untrack(() => (standing.length > 0 ? 'publish' : 'private')));
  let picked = $state<Record<number, boolean>>(
    untrack(() => Object.fromEntries(standing.map((f) => [f.id, pickedByDefault(f)]))),
  );
  let texts = $state<Record<number, string>>(
    untrack(() => Object.fromEntries(standing.map((f) => [f.id, commentDraft(f)]))),
  );
  let summary = $state('');
  let event = $state<ReviewEvent>('comment');

  const draft = $derived<ReviewDraft>({
    comments: standing
      .filter((f) => picked[f.id])
      .map((f) => ({ finding: f.id, body: texts[f.id] ?? '' })),
    summary,
    event,
  });

  let preview = $state<PublishPreview | null>(null);
  let previewed = $state<ReviewDraft | null>(null);
  /** Whether the preview shown is of the draft as it stands. */
  const current = $derived(
    preview !== null && previewed !== null && sameDraft(previewed, draft),
  );
  let working = $state(false);
  let problem = $state<string | null>(null);
  /** Set when approving was refused because the pull request moved past the reviewed commit. */
  let stale = $state<{ reviewed: string; current: string } | null>(null);
  let confirming = $state(false);
  const own = $derived(preview?.ownPullRequest ?? false);
  /** The stale block sits above the options, which the sheet has usually scrolled past by then. */
  let staleBlock = $state<HTMLElement | null>(null);
  $effect(() => {
    if (stale && staleBlock) staleBlock.scrollIntoView({ block: 'nearest' });
  });

  function said(error: unknown): string {
    return isWtmError(error) ? error.message : String(error);
  }

  async function showPreview(): Promise<void> {
    if (working) return;
    working = true;
    problem = null;
    try {
      const asked = $state.snapshot(draft);
      preview = await onpreview(asked);
      previewed = asked;
      if (preview.ownPullRequest && event !== 'comment') event = 'comment';
    } catch (error) {
      problem = said(error);
    } finally {
      working = false;
    }
  }

  async function approve(againstHead: string | null): Promise<void> {
    if (working || !preview || !previewed) return;
    working = true;
    problem = null;
    try {
      await onapprove($state.snapshot(previewed), preview.hash, againstHead);
      onclose();
    } catch (error) {
      if (isWtmError(error) && error.kind === 'headMoved') {
        const detail = error.detail as { reviewed?: string; current?: string } | null;
        stale =
          detail?.reviewed && detail.current
            ? { reviewed: detail.reviewed, current: detail.current }
            : null;
        problem = stale ? null : error.message;
      } else {
        problem = said(error);
      }
    } finally {
      working = false;
      confirming = false;
    }
  }

  async function submit(e: Event): Promise<void> {
    e.preventDefault();
    if (busy || working) return;
    if (choice === 'private') {
      if (await onkeep(notes)) onclose();
    } else if (current) {
      await approve(null);
    } else {
      await showPreview();
    }
  }

  const repo = $derived(pr ? `${pr.base.repo.owner}/${pr.base.repo.name}` : '');
</script>

<Dialog
  title={`Finish your review of #${review?.number ?? ''}`}
  {onclose}
  onsubmit={submit}
  closeDisabled={busy || working}
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

      {#if stale}
        <section
          bind:this={staleBlock}
          class="c-work-state c-work-state--warn"
          aria-labelledby="pr-outcome-stale"
        >
          <span class="c-work-state__eyebrow">Approval is stale</span>
          <h3 class="c-work-state__title" id="pr-outcome-stale">
            #{review?.number} changed after you reviewed it
          </h3>
          <p class="c-work-state__prose">
            Nothing was posted. Your review covered commit <code
              >{short(stale.reviewed)}</code
            >; the pull request is at <code>{short(stale.current)}</code> now. Re-review the
            changes, or publish what you previewed against {short(stale.reviewed)}: GitHub
            marks a comment whose line changed since as outdated.
          </p>
          <div class="c-work-state__actions">
            {#if confirming}
              <Button
                variant="accent"
                size="sm"
                disabled={working}
                onclick={() => void approve(stale?.current ?? null)}
                >Publish against {short(stale.reviewed)}</Button
              >
              <Button size="sm" disabled={working} onclick={() => (confirming = false)}
                >Back</Button
              >
            {:else}
              <Button
                variant="accent"
                size="sm"
                disabled={working}
                onclick={() => {
                  if (stale) onrereview(stale.current);
                  onclose();
                }}>Re-review the changes</Button
              >
              <Button size="sm" disabled={working} onclick={() => (confirming = true)}
                >Publish against {short(stale.reviewed)} anyway…</Button
              >
              <Button
                variant="quiet"
                size="sm"
                disabled={working}
                onclick={() => (stale = null)}>Cancel</Button
              >
            {/if}
          </div>
        </section>
      {/if}

      <div class="c-pr-outcome__options" role="radiogroup" aria-label="Outcome">
        <div class="c-pr-outcome__option" class:is-selected={choice === 'private'}>
          <label class="c-pr-outcome__choice">
            <input type="radio" name="pr-outcome" value="private" bind:group={choice} />
            <span class="c-pr-outcome__text">
              <span class="c-pr-outcome__title">Keep private notes</span>
              <span class="c-pr-outcome__desc"
                >Findings stay in wtm. Nothing is posted to GitHub or Jira.</span
              >
            </span>
          </label>
          {#if choice === 'private'}
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
              {standing.length === 1
                ? '1 finding stays'
                : `${standing.length} findings stay`} beside the notes{reviewed
                ? `, pinned to ${short(reviewed)}`
                : ''}.{moved
                ? ' The pull request has moved on since; Re-review changes first if you want notes on the new head.'
                : ''}
            </p>
          {/if}
        </div>

        <div class="c-pr-outcome__option" class:is-selected={choice === 'publish'}>
          <label class="c-pr-outcome__choice">
            <input type="radio" name="pr-outcome" value="publish" bind:group={choice} />
            <span class="c-pr-outcome__text">
              <span class="c-pr-outcome__title">Publish a review</span>
              <span class="c-pr-outcome__desc"
                >You choose exactly what is posted. Agent wording is a draft; edit it first.</span
              >
            </span>
          </label>
          {#if choice === 'publish'}
            <fieldset class="c-publication__picks">
              <legend class="c-publication__legend">Inline comments</legend>
              {#each standing as finding (finding.id)}
                <div class="c-publication__pick" class:is-off={!picked[finding.id]}>
                  <label class="c-publication__pick-row">
                    <input
                      type="checkbox"
                      bind:checked={picked[finding.id]}
                      disabled={working}
                    />
                    <span
                      class="c-publication__severity"
                      class:is-blocking={finding.severity === 'blocking'}
                      class:is-notable={finding.severity === 'notable'}
                      >{finding.class.kind === 'rule_violation'
                        ? 'Rule'
                        : SEVERITY_WORDS[finding.severity]}</span
                    >
                    <code>{findingPlace(finding)}</code>
                    <CodeText text={finding.title} />
                    {#if !picked[finding.id] && finding.class.kind === 'suggestion'}
                      <span>(kept private)</span>
                    {/if}
                  </label>
                  {#if picked[finding.id]}
                    <textarea
                      class="c-textarea c-publication__edit"
                      rows="3"
                      aria-label={`Comment on ${findingPlace(finding)}`}
                      bind:value={texts[finding.id]}
                      disabled={working}></textarea>
                  {/if}
                </div>
              {:else}
                <p class="c-pr-outcome__note">
                  This review has no open findings to comment on.
                </p>
              {/each}
            </fieldset>
            <label class="c-pr-outcome__field">
              <span class="c-pr-outcome__label">Summary comment</span>
              <textarea
                class="c-textarea"
                rows="4"
                bind:value={summary}
                disabled={working}
                placeholder="What you want the author to read first"></textarea>
            </label>
            <div class="c-publication__states" role="radiogroup" aria-label="Review state">
              <span class="c-pr-outcome__label">Review state</span>
              {#each ['comment', 'request_changes', 'approve'] as const as state (state)}
                <label
                  class="c-publication__state-label"
                  title={own && state !== 'comment'
                    ? 'GitHub does not let you approve or request changes on your own pull request'
                    : undefined}
                >
                  <input
                    type="radio"
                    name="review-state"
                    value={state}
                    bind:group={event}
                    disabled={working || (own && state !== 'comment')}
                  />
                  {EVENT_WORDS[state]}
                </label>
              {/each}
              <Button size="sm" disabled={working} onclick={() => void showPreview()}
                >{working && !current
                  ? 'Reading GitHub…'
                  : 'Preview exactly what posts'}</Button
              >
            </div>

            {#if preview}
              <section
                class="c-publication__preview"
                class:is-outdated={!current}
                aria-label="What posts"
              >
                <p class="c-publication__note">
                  {#if current}
                    Exactly what posts{preview.account ? ` as @${preview.account}` : ''} to {repo}
                    #{preview.review.number}, pinned to commit
                    <code>{short(preview.review.commitId)}</code>, as
                    <strong>{EVENT_WORDS[preview.review.event]}</strong>.
                  {:else}
                    You changed something since this preview. Preview again before
                    approving.
                  {/if}
                </p>
                {#each preview.comments as comment (comment.finding)}
                  <div class="c-publication__post">
                    <span class="c-publication__where"
                      >{comment.path}:{comment.startLine !== null
                        ? `${comment.startLine}-${comment.line}`
                        : comment.line}</span
                    >
                    <pre class="c-publication__body">{comment.body}</pre>
                  </div>
                {/each}
                {#each preview.review.folded as folded (folded.finding)}
                  <p class="c-publication__note">
                    <code
                      >{folded.path ?? 'The whole change'}{folded.line !== null
                        ? `:${folded.line}`
                        : ''}</code
                    >
                    goes in the summary: {FOLD_WORDS[folded.reason]}.
                  </p>
                {/each}
                <div class="c-publication__post">
                  <span class="c-publication__where"
                    >Summary, submitted with the review</span
                  >
                  <pre class="c-publication__body">{preview.body}</pre>
                </div>
                <p class="c-publication__note">
                  The comments go on a pending review first (its own body is only its
                  marker,
                  <code>{preview.pendingBody}</code>), which is then submitted with the
                  summary. Each <code>&lt;!-- wtm:… --&gt;</code> line is an HTML comment GitHub
                  does not show; it is how wtm finds what it posted and never posts it twice.
                </p>
              </section>
            {/if}
          {/if}
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
      {#if problem ?? error}
        <p class="c-work-state__error" role="alert">
          <CodeText text={problem ?? error ?? ''} />
        </p>
      {/if}
    </div>
  {/snippet}
  {#snippet footer()}
    {#if choice === 'private'}
      <p class="c-pr-outcome__summary">
        Kept in wtm only{reviewed ? `, pinned to ${short(reviewed)}` : ''}. Nothing is sent
        anywhere.
      </p>
      <Button onclick={onclose} disabled={busy}>Cancel</Button>
      <Button variant="accent" type="submit" disabled={busy}>
        {busy ? 'Keeping…' : 'Keep private notes'}
      </Button>
    {:else}
      <p class="c-pr-outcome__summary">
        {review && reviewed
          ? postsLine(draft, repo, review.number, reviewed, preview?.account ?? null)
          : ''}
      </p>
      <Button onclick={onclose} disabled={working}>Cancel</Button>
      <Button
        variant="accent"
        type="submit"
        disabled={working || !current || stale !== null}
        title={current ? undefined : 'Preview exactly what posts first'}
      >
        {working && current ? 'Approving…' : 'Approve and publish'}
      </Button>
    {/if}
  {/snippet}
</Dialog>
