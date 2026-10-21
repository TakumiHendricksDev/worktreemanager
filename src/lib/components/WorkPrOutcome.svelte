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
   * edit after it, or a finding that changed under it, needs a new preview.
   *
   * # Fix it locally, then choose where it goes
   *
   * Pick findings that name a file and a writer role, and Start the fix: the writer gets the review
   * worktree to itself (the writer lease) and may change only those findings' files; wtm commits
   * for it. While it works the sheet says so. Once its fix is ready the option shows the diff — the
   * files, anything left uncommitted, and the patch to read — and the three routes, each with
   * exactly where it goes or why it is closed: a follow-up pull request (the default), suggestion
   * comments, or a push to their branch. **Preview exactly what is sent** shows the commands and
   * the pull request's text, or each suggestion as it posts, and **Approve and send** is open only
   * while that preview is of the route as it stands.
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
    ApprovedRoute,
    FixDiffView,
    PublicationView,
    PublishPreview,
    PullRequest,
    ReviewDraft,
    ReviewEvent,
    ReviewRunView,
    RouteKind,
    RouteOptions,
    RoutePreview,
    RouteRequest,
    StartFix,
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
  import {
    currentFix,
    fixToSend,
    leaseLine,
    ROUTE_ORDER,
    ROUTE_WORDS,
    sameRequest,
  } from '../local-fix';
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
    onstartfix,
    onfixdiff,
    onroutes,
    onpreviewroute,
    onapproveroute,
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
    /** Start a local fix. Rejects with the app's error. */
    onstartfix: (request: StartFix) => Promise<void>;
    /** The current fix's diff. */
    onfixdiff: () => Promise<FixDiffView>;
    /** Every route for the fix, from a fresh read. */
    onroutes: () => Promise<RouteOptions>;
    /** Exactly what a route would send. */
    onpreviewroute: (request: RouteRequest) => Promise<RoutePreview>;
    /** Approve what was previewed, by its hash. Rejects with the app's error. */
    onapproveroute: (request: RouteRequest, hash: string) => Promise<ApprovedRoute>;
  } = $props();

  type Choice = 'private' | 'publish' | 'fix';

  const review = $derived(view.pullRequest);
  const pr = $derived(read ?? review?.mirror?.pullRequest ?? null);
  const reviewed = $derived(review?.reviewedHead ?? null);
  const now = $derived(review ? currentHead(review, read) : null);
  const moved = $derived(reviewed !== null && now !== null && reviewed !== now);
  const push = $derived(review?.pushToBranch ?? null);
  const standing = $derived(view.findings.filter((f) => f.dismissed === null));
  const earlier = $derived(review ? currentOutcome(review) : null);

  const fix = $derived(currentFix(view));
  /** The current fix, when it finished with commits to send. */
  const ready = $derived(fixToSend(view));
  const writing = $derived(fix?.state === 'writing');
  /** Findings a fix can be held to: the ones that name a file. */
  const fixable = $derived(standing.filter((f) => f.location !== null));

  // Read once, when the sheet opens; what you type and pick is yours from then on.
  let notes = $state(untrack(() => earlier?.notes ?? ''));
  let choice = $state<Choice>(
    untrack(() =>
      fixToSend(view) || currentFix(view)?.state === 'writing'
        ? 'fix'
        : standing.length > 0
          ? 'publish'
          : 'private',
    ),
  );
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
    } else if (choice === 'fix') {
      if (ready) {
        if (routeCurrent) await approveRoute();
        else await previewRoute();
      } else if (!writing) {
        await startFix();
      }
    } else if (current) {
      await approve(null);
    } else {
      await showPreview();
    }
  }

  const repo = $derived(pr ? `${pr.base.repo.owner}/${pr.base.repo.name}` : '');

  // ── fix it locally ──

  let fixPicks = $state<Record<number, boolean>>(
    untrack(() =>
      Object.fromEntries(
        standing.filter((f) => f.location !== null).map((f) => [f.id, pickedByDefault(f)]),
      ),
    ),
  );
  let writer = $state(untrack(() => view.writers[0]?.id ?? ''));
  const fixRequest = $derived<StartFix>({
    findings: fixable.filter((f) => fixPicks[f.id]).map((f) => f.id),
    role: writer,
  });

  async function startFix(): Promise<void> {
    if (working) return;
    working = true;
    problem = null;
    try {
      await onstartfix($state.snapshot(fixRequest));
      onclose();
    } catch (error) {
      problem = said(error);
    } finally {
      working = false;
    }
  }

  let diff = $state<FixDiffView | null>(null);
  let routes = $state<RouteOptions | null>(null);
  let reading = $state(false);
  let routeKind = $state<RouteKind>('follow_up');
  let routeTitle = $state('');
  let routeBody = $state('');
  let summary2 = $state('');
  const routeRequest = $derived<RouteRequest>({
    kind: routeKind,
    title: routeKind === 'follow_up' ? routeTitle : '',
    body:
      routeKind === 'follow_up' ? routeBody : routeKind === 'suggestions' ? summary2 : '',
  });
  let routePreview = $state<RoutePreview | null>(null);
  let routePreviewed = $state<RouteRequest | null>(null);
  const routeCurrent = $derived(
    routePreview !== null &&
      routePreviewed !== null &&
      sameRequest(routePreviewed, routeRequest),
  );
  const chosenRoute = $derived(routes?.routes.find((r) => r.kind === routeKind) ?? null);

  /** The diff and the routes, read once the fix option is open on a ready fix. */
  $effect(() => {
    if (choice !== 'fix' || !ready || diff || reading) return;
    untrack(() => void loadFix());
  });

  async function loadFix(): Promise<void> {
    reading = true;
    try {
      const [readDiff, readRoutes] = await Promise.all([onfixdiff(), onroutes()]);
      diff = readDiff;
      routes = readRoutes;
      const follow = readRoutes.routes.find((r) => r.kind === 'follow_up');
      routeTitle = follow?.title ?? '';
      routeBody = follow?.body ?? '';
      summary2 = readRoutes.routes.find((r) => r.kind === 'suggestions')?.body ?? '';
      // The default is the follow-up; when it is closed, the first route that is open.
      const open = ROUTE_ORDER.find(
        (kind) => readRoutes.routes.find((r) => r.kind === kind)?.unavailable === null,
      );
      if (open) routeKind = open;
    } catch (error) {
      problem = said(error);
    } finally {
      reading = false;
    }
  }

  async function previewRoute(): Promise<void> {
    if (working) return;
    working = true;
    problem = null;
    try {
      const asked = $state.snapshot(routeRequest);
      routePreview = await onpreviewroute(asked);
      routePreviewed = asked;
    } catch (error) {
      problem = said(error);
    } finally {
      working = false;
    }
  }

  async function approveRoute(): Promise<void> {
    if (working || !routePreview || !routePreviewed) return;
    working = true;
    problem = null;
    try {
      await onapproveroute($state.snapshot(routePreviewed), routePreview.hash);
      onclose();
    } catch (error) {
      problem = said(error);
    } finally {
      working = false;
    }
  }
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

        <div class="c-pr-outcome__option" class:is-selected={choice === 'fix'}>
          <label class="c-pr-outcome__choice">
            <input type="radio" name="pr-outcome" value="fix" bind:group={choice} />
            <span class="c-pr-outcome__text">
              <span class="c-pr-outcome__title"
                >Fix it locally, then choose where it goes</span
              >
              <span class="c-pr-outcome__desc"
                >A writer fixes the selected findings in the review worktree, and you read
                the diff before anything leaves this Mac.</span
              >
            </span>
          </label>
          {#if choice !== 'fix' && pr}
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
          {:else if choice === 'fix' && writing && fix}
            <p class="c-pr-outcome__note">
              <code>{fix.role}</code> is fixing {fix.findings.length === 1
                ? '1 finding'
                : `${fix.findings.length} findings`} now, in {fix.scope.paths
                .map((path) => `\`${path}\``)
                .join(', ')} only. Close this sheet; the Reviews tab shows its progress, and Stop
              fix ends it.
            </p>
          {:else if choice === 'fix' && ready}
            <section class="c-local-fix" aria-label="The fix">
              {#if reading && !diff}
                <p class="c-pr-outcome__note">Reading the fix and the pull request…</p>
              {/if}
              {#if diff}
                <p class="c-local-fix__head">
                  <code>{ready.role}</code> fixed {ready.findings.length === 1
                    ? '1 finding'
                    : `${ready.findings.length} findings`} as
                  <code>{short(diff.head)}</code> on {short(diff.base)} · diff
                  <code>{diff.hash.replace('sha256:', '').slice(0, 12)}</code>
                </p>
                <ul class="o-plain-list c-local-fix__files" aria-label="Files">
                  {#each diff.files as file (file.path)}
                    <li class="c-local-fix__file">
                      <code>{file.path}</code>
                      {#if file.added !== null}<span class="c-local-fix__added"
                          >+{file.added}</span
                        >{/if}
                      {#if file.removed !== null}<span class="c-local-fix__removed"
                          >−{file.removed}</span
                        >{/if}
                    </li>
                  {/each}
                </ul>
                {#if diff.uncommitted.length > 0}
                  <p class="c-local-fix__left" role="note">
                    <CodeText
                      text={`Not in the fix: ${diff.uncommitted.map((p) => `\`${p}\``).join(', ')} ${
                        diff.outliers.length > 0
                          ? "— outside the findings' files, waiting on your answer in the inbox."
                          : 'left uncommitted.'
                      } Only what is committed is sent.`}
                    />
                  </p>
                {/if}
                <details class="c-local-fix__read">
                  <summary>Read the diff</summary>
                  <pre class="c-local-fix__patch">{diff.patch}</pre>
                  {#if diff.truncated}
                    <p class="c-pr-outcome__note">
                      Cut short here; the approval covers the whole diff.
                    </p>
                  {/if}
                </details>
              {/if}
              {#if routes}
                <fieldset class="c-local-fix__routes">
                  <legend class="c-publication__legend">Where it goes</legend>
                  {#each ROUTE_ORDER as kind (kind)}
                    {@const option = routes.routes.find((r) => r.kind === kind)}
                    <label
                      class="c-local-fix__route"
                      class:is-unavailable={option?.unavailable != null}
                    >
                      <input
                        type="radio"
                        name="fix-route"
                        value={kind}
                        bind:group={routeKind}
                        disabled={working || option?.unavailable != null}
                      />
                      <span class="c-pr-outcome__text">
                        <span class="c-local-fix__route-title"
                          >{ROUTE_WORDS[kind].title}{kind === 'follow_up'
                            ? ' (default)'
                            : ''}</span
                        >
                        <span class="c-pr-outcome__desc">{ROUTE_WORDS[kind].desc}</span>
                        {#if option?.destination}
                          <span class="c-local-fix__where"
                            ><CodeText text={option.destination} /></span
                          >
                        {:else if option?.unavailable}
                          <span class="c-local-fix__why"
                            >Unavailable: <CodeText text={option.unavailable} /></span
                          >
                        {/if}
                      </span>
                    </label>
                  {/each}
                </fieldset>
                {#if routeKind === 'follow_up' && chosenRoute?.unavailable === null}
                  <label class="c-pr-outcome__field">
                    <span class="c-pr-outcome__label">Pull request title</span>
                    <input class="c-input" bind:value={routeTitle} disabled={working} />
                  </label>
                  <label class="c-pr-outcome__field">
                    <span class="c-pr-outcome__label">Pull request description</span>
                    <textarea
                      class="c-textarea"
                      rows="5"
                      bind:value={routeBody}
                      disabled={working}></textarea>
                  </label>
                {:else if routeKind === 'suggestions' && chosenRoute?.unavailable === null}
                  <label class="c-pr-outcome__field">
                    <span class="c-pr-outcome__label">Summary comment</span>
                    <textarea
                      class="c-textarea"
                      rows="3"
                      bind:value={summary2}
                      disabled={working}></textarea>
                  </label>
                {/if}
                <div class="c-publication__states">
                  <Button
                    size="sm"
                    disabled={working || chosenRoute?.unavailable !== null}
                    onclick={() => void previewRoute()}
                    >{working && !routeCurrent
                      ? 'Reading…'
                      : 'Preview exactly what is sent'}</Button
                  >
                </div>
              {/if}
              {#if routePreview}
                <section
                  class="c-publication__preview"
                  class:is-outdated={!routeCurrent}
                  aria-label="What is sent"
                >
                  <p class="c-publication__note">
                    {#if routeCurrent}
                      Exactly what is sent, in order, for the diff
                      <code
                        >{routePreview.diffHash.replace('sha256:', '').slice(0, 12)}</code
                      >, with their branch at <code>{short(routePreview.head)}</code>. If it
                      moves before the push, wtm stops and asks again.
                    {:else}
                      You changed something since this preview. Preview again before
                      approving.
                    {/if}
                  </p>
                  {#each routePreview.steps as step, index (index)}
                    <pre class="c-publication__body">{step}</pre>
                  {/each}
                  {#if routePreview.plan?.open}
                    <div class="c-publication__post">
                      <span class="c-publication__where"
                        >Pull request: {routePreview.plan.open.title}</span
                      >
                      <pre class="c-publication__body">{routePreview.body}</pre>
                    </div>
                  {/if}
                  {#if routePreview.suggestions}
                    {#each routePreview.suggestions.comments as comment, index (index)}
                      <div class="c-publication__post">
                        <span class="c-publication__where"
                          >{comment.path}:{comment.startLine !== null
                            ? `${comment.startLine}-${comment.line}`
                            : comment.line}</span
                        >
                        <pre class="c-publication__body">{comment.body}</pre>
                      </div>
                    {/each}
                    <div class="c-publication__post">
                      <span class="c-publication__where">Summary, submitted with them</span>
                      <pre class="c-publication__body">{routePreview.suggestions.body}</pre>
                    </div>
                  {/if}
                </section>
              {/if}
            </section>
          {:else if choice === 'fix'}
            {#if fix && fix.state !== 'ready'}
              <p class="c-pr-outcome__note">
                The last fix {fix.state === 'failed'
                  ? 'did not finish'
                  : 'was stopped'}{fix.detail ? `: ${fix.detail}` : '.'} Whatever it committed
                is in the review worktree.
              </p>
            {/if}
            {#if leaseLine(view.leases)}
              <p class="c-pr-outcome__note">
                <CodeText text={leaseLine(view.leases) ?? ''} />
              </p>
            {/if}
            <fieldset class="c-publication__picks">
              <legend class="c-publication__legend">Findings to fix</legend>
              {#each fixable as finding (finding.id)}
                <label class="c-publication__pick-row">
                  <input
                    type="checkbox"
                    bind:checked={fixPicks[finding.id]}
                    disabled={working}
                  />
                  <span
                    class="c-publication__severity"
                    class:is-blocking={finding.severity === 'blocking'}
                    class:is-notable={finding.severity === 'notable'}
                    >{SEVERITY_WORDS[finding.severity]}</span
                  >
                  <code>{findingPlace(finding)}</code>
                  <CodeText text={finding.title} />
                </label>
              {:else}
                <p class="c-pr-outcome__note">
                  No open finding names a file, so there is nothing a fix could be held to.
                </p>
              {/each}
              {#if standing.length > fixable.length}
                <p class="c-pr-outcome__note">
                  Findings about the change as a whole name no file to hold a writer to;
                  publish them as comments instead.
                </p>
              {/if}
            </fieldset>
            {#if view.writers.length > 0}
              <label class="c-pr-outcome__field">
                <span class="c-pr-outcome__label">Writer</span>
                <select class="c-select" bind:value={writer} disabled={working}>
                  {#each view.writers as choice (choice.id)}
                    <option value={choice.id}
                      >{choice.id} · {choice.provider}{choice.model
                        ? ` ${choice.model}`
                        : ''}</option
                    >
                  {/each}
                </select>
              </label>
              <p class="c-pr-outcome__note">
                The writer may change only the picked findings' files. It cannot commit or
                push: wtm commits for it, with your git config and signing and with hooks
                off, and a file outside the scope becomes a question in the inbox.
              </p>
            {:else}
              <p class="c-pr-outcome__note">
                No writer role yet: add a <code>[workflow.role.&lt;id&gt;]</code> with
                <code>profile = "writer"</code> to your own config.
              </p>
            {/if}
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
    {:else if choice === 'fix'}
      <p class="c-pr-outcome__summary">
        {#if ready}
          <CodeText
            text={chosenRoute?.destination ??
              'Choose where the fix goes. Nothing is sent before you approve it.'}
          />
        {:else if writing}
          The writer has the review worktree until it is done.
        {:else}
          Nothing leaves this Mac: the fix stays in the review worktree until you choose
          where it goes.
        {/if}
      </p>
      <Button onclick={onclose} disabled={working}>Cancel</Button>
      {#if ready}
        <Button
          variant="accent"
          type="submit"
          disabled={working || !routeCurrent}
          title={routeCurrent ? undefined : 'Preview exactly what is sent first'}
        >
          {working && routeCurrent ? 'Approving…' : 'Approve and send'}
        </Button>
      {:else if !writing}
        <Button
          variant="accent"
          type="submit"
          disabled={working ||
            fixRequest.findings.length === 0 ||
            !fixRequest.role ||
            view.leases.length > 0}
        >
          {working ? 'Starting…' : 'Start the fix'}
        </Button>
      {/if}
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
