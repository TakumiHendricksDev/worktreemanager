<script lang="ts">
  /**
   * A queued pull request in the Work view's main column: what the forge says about it, and the
   * worktree a review of it reads.
   *
   * # What the forge said, and when
   *
   * Read when the row is opened, and again on Refresh, never on a timer (ARCHITECTURE §8). The
   * facts are the ones a review decides on before reading a line: whose branch it is and at which
   * commit, whether it is a fork and whether its author lets maintainers push, what you may do on
   * the repository, who has been asked, how the checks stand, and whether the base is protected.
   *
   * # Preparing the worktree
   *
   * The preview is the create pipeline's, so it says exactly what will run before anything does:
   * the fetch, the `git worktree add` argv, and what happens about setup. "Prepare environment" is
   * unticked every time the panel opens, because the project's setup may clone a database or run
   * migrations, and this is somebody else's branch. The head the panel showed travels with the
   * click, so a push in between is refused rather than checked out unseen.
   *
   * When a worktree already holds the pull request, the preview says so and Prepare hands that one
   * back. Setup output is not streamed here: the Work view draws no terminal, and a setup session
   * opted into is the worktree's, readable from the worktree view.
   *
   * # Two tabs, as a worktree has
   *
   * Overview is the forge's facts and the Prepare panel. Reviews is `WorkReviewRun` in its pull
   * request mode: Run Review on the review worktree against the pull request's base, following the
   * pull request to new heads, the heads it left, and the outcome sheet. Both read the same
   * reading of the pull request, so the two cannot disagree about where its head is.
   *
   * # "I'm reviewing this"
   *
   * A flag in wtm, in the header because it is about the pull request rather than either tab.
   * GitHub's requested reviewers are shown on Overview as the forge has them and are never changed.
   */
  import { commands } from '../ipc/commands';
  import {
    errorMessage,
    type ReviewCheckout,
    type ReviewItem,
    type ReviewOutcome,
    type ReviewPreview,
    type ReviewWorktreeArgs,
  } from '../ipc/types';
  import { reviews } from '../state/reviews.svelte';
  import { work, type WorkTab } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import {
    checksLine,
    forkLine,
    headLabel,
    PERMISSION_WORDS,
    PR_STATE_WORDS,
    protectionLine,
    REUSE_WORDS,
    setupLine,
  } from '../work';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import PreflightList from './ui/PreflightList.svelte';
  import WorkReviewRun from './WorkReviewRun.svelte';

  const {
    item,
    visible,
    onopenworktree,
  }: {
    item: ReviewItem;
    /** Whether the panel is on screen: Work is up and its Inbox is not over it. */
    visible: boolean;
    /** Leave Work for this worktree's sessions. */
    onopenworktree: (worktreeId: string) => void;
  } = $props();

  const projectId = $derived(workspace.activeProjectId);
  const detail = $derived(reviews.details[item.id] ?? null);
  const detailError = $derived(reviews.detailErrors[item.id] ?? null);
  const reading = $derived(reviews.reading.includes(item.id));
  const pr = $derived(detail?.pullRequest ?? null);
  const listed = $derived(
    item.worktree === null
      ? null
      : (workspace.worktrees.find((w) => w.path === item.worktree && w.prunable === null) ??
          null),
  );

  let checkout = $state<ReviewCheckout>('detached');
  let prepareEnvironment = $state(false);
  let acknowledged = $state<string[]>([]);
  let preview = $state<ReviewPreview | null>(null);
  let previewError = $state<string | null>(null);
  let preparing = $state(false);
  let outcome = $state<ReviewOutcome | null>(null);
  let prepareError = $state<string | null>(null);
  /** Bumped after Prepare, so the preview is asked again and says the worktree is there now. A
   *  queue reload alone does not ask: it hands over a new `item` for the same pull request. */
  let revision = $state(0);

  // A different pull request is a fresh panel: the opt-in in particular never carries over. Keyed
  // on the id through a derived, because reloading the queue after Prepare hands this component a
  // new `item` object for the same pull request, and that must not wipe the outcome it just showed.
  const itemId = $derived(item.id);
  $effect.pre(() => {
    void itemId;
    checkout = 'detached';
    prepareEnvironment = false;
    acknowledged = [];
    preview = null;
    previewError = null;
    outcome = null;
    prepareError = null;
  });

  // Opened without a reading, or with one from before an add elsewhere: read it now.
  $effect(() => {
    if (projectId && !detail && !reading && !detailError)
      void reviews.refresh(projectId, item.id);
  });

  /** The request, without acknowledgements: a preview does not read them, so ticking one should
   *  not ask the forge again. */
  function args(): ReviewWorktreeArgs | null {
    if (!pr) return null;
    void revision;
    return { itemId, expectedHead: pr.head.sha, checkout, prepareEnvironment };
  }

  // Previewed whenever what it depends on changes. A token drops answers that arrive after a newer
  // question was asked, so a slow `gh` cannot paint the preview of the option you just left.
  let asked = 0;
  $effect(() => {
    const request = args();
    const project = projectId;
    if (!request || !project) return;
    const token = ++asked;
    previewError = null;
    commands
      .previewReviewWorktree(project, request)
      .then((answer) => {
        if (token === asked) preview = answer;
      })
      .catch((error: unknown) => {
        if (token === asked) {
          preview = null;
          previewError = errorMessage(error);
        }
      });
  });

  /** Every error waived where it may be, and none that may not. */
  const ready = $derived(
    preview !== null &&
      preview.preflight.every(
        (p) => p.severity !== 'error' || (p.overridable && acknowledged.includes(p.id)),
      ),
  );

  async function prepare(): Promise<void> {
    const request = args();
    if (!request || !projectId || preparing) return;
    // A snapshot, not the state proxy: what crosses IPC should be plain data.
    request.acknowledged = $state.snapshot(acknowledged);
    preparing = true;
    prepareError = null;
    try {
      outcome = await commands.createReviewWorktree(projectId, request);
      await Promise.all([workspace.refreshWorktrees(), reviews.load(projectId)]);
      revision += 1;
    } catch (error) {
      prepareError = errorMessage(error);
    } finally {
      preparing = false;
    }
  }

  function acknowledge(id: string, on: boolean): void {
    acknowledged = on ? [...acknowledged, id] : acknowledged.filter((a) => a !== id);
  }

  const TABS: { id: WorkTab; label: string }[] = [
    { id: 'overview', label: 'Overview' },
    { id: 'reviews', label: 'Reviews' },
  ];
  let tabs = $state<HTMLElement | undefined>();

  /** Arrow-key roving across the strip, as the worktree workspace's tabs move. */
  function onTabKey(event: KeyboardEvent, current: WorkTab): void {
    const ids = TABS.map((t) => t.id);
    const at = ids.indexOf(current);
    let next = at;
    if (event.key === 'ArrowRight') next = at + 1;
    else if (event.key === 'ArrowLeft') next = at - 1;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = ids.length - 1;
    else return;
    event.preventDefault();
    const id = ids[Math.min(Math.max(next, 0), ids.length - 1)];
    if (id) work.tab = id;
    queueMicrotask(() =>
      tabs?.querySelector<HTMLElement>('[aria-selected="true"]')?.focus(),
    );
  }

  const reusing = $derived(preview?.target.kind === 'reuse');
  const openable = $derived(
    outcome && outcome.kind !== 'cancelled' ? outcome.worktreeId : (listed?.id ?? null),
  );
</script>

<header class="c-work__header">
  <div class="c-work__heading">
    <h1 class="c-work__title" title={pr?.title ?? item.title}>
      #{item.number} · {pr?.title ?? item.title}
    </h1>
    <p class="c-work__meta">
      <span class="u-mono">
        {pr
          ? `${headLabel(pr)} → ${pr.base.repo.owner}/${pr.base.repo.name} ${pr.base.branch}`
          : item.repo}
      </span>
      {#if pr}<span>{pr.author ? `By ${pr.author}` : 'Author unknown'}</span>{/if}
    </p>
  </div>
  <div class="c-work__actions">
    <span
      class="c-work-review__claim"
      title="Kept in wtm. GitHub's requested reviewers are not changed."
    >
      <Choice
        checked={item.reviewingSince !== null}
        disabled={!projectId}
        onchange={(on) => projectId && void reviews.setReviewing(projectId, item.id, on)}
      >
        I'm reviewing this
      </Choice>
    </span>
    {#if pr}
      <span class="c-badge">{pr.draft ? 'Draft' : PR_STATE_WORDS[pr.state]}</span>
    {/if}
    <Button
      size="sm"
      disabled={reading || !projectId}
      onclick={() => projectId && reviews.refresh(projectId, item.id)}
    >
      {reading ? 'Reading…' : 'Refresh'}
    </Button>
  </div>
</header>

<div class="c-tabs c-tabs--inset" role="tablist" aria-label="Pull request" bind:this={tabs}>
  {#each TABS as tab (tab.id)}
    <button
      class="c-tabs__tab"
      class:is-active={work.tab === tab.id}
      role="tab"
      id="work-pr-tab-{tab.id}"
      aria-selected={work.tab === tab.id}
      aria-controls="work-pr-panel"
      tabindex={work.tab === tab.id ? 0 : -1}
      onclick={() => (work.tab = tab.id)}
      onkeydown={(event) => onTabKey(event, tab.id)}
    >
      {tab.label}
    </button>
  {/each}
</div>

{#if work.tab === 'reviews'}
  <div
    class="c-work__panel"
    id="work-pr-panel"
    role="tabpanel"
    aria-labelledby="work-pr-tab-reviews"
  >
    {#if detailError}
      <p class="c-work-review__error c-status--danger" role="alert">{detailError}</p>
    {/if}
    {#if listed && projectId}
      <WorkReviewRun
        {visible}
        {projectId}
        worktree={listed}
        reviewing={`#${item.number}`}
        subject={`#${item.number}`}
        pullRequest={{ item, read: pr }}
      />
    {:else}
      <section class="c-work-state" aria-labelledby="work-pr-unprepared">
        <span class="c-work-state__eyebrow">Not reviewed yet</span>
        <h2 class="c-work-state__title" id="work-pr-unprepared">
          #{item.number} has no review worktree
        </h2>
        <p class="c-work-state__prose">
          A review reads the pull request's head in a worktree of its own. Prepare one on
          Overview: it checks out the head commit, and runs this project's setup only if you
          ask.
        </p>
        <div class="c-work-state__actions">
          <Button variant="accent" onclick={() => (work.tab = 'overview')}
            >Prepare a review worktree</Button
          >
        </div>
      </section>
    {/if}
  </div>
{:else}
  <div
    class="c-work__panel"
    id="work-pr-panel"
    role="tabpanel"
    aria-labelledby="work-pr-tab-overview"
  >
    {#if detailError}
      <p class="c-work-review__error c-status--danger" role="alert">{detailError}</p>
    {/if}

    {#if pr && detail}
      <dl class="o-facts o-facts--wide c-work-review__facts">
        <dt>Head</dt>
        <dd>
          <span class="u-mono">{headLabel(pr)}</span> at
          <span class="u-mono">{pr.head.sha.slice(0, 7)}</span>
        </dd>
        <dt>Base</dt>
        <dd>
          <span class="u-mono"
            >{pr.base.repo.owner}/{pr.base.repo.name} {pr.base.branch}</span
          >
          {#if detail.baseIntegration}<span class="c-badge">Integration branch</span>{/if}
        </dd>
        <dt>Fork</dt>
        <dd>{forkLine(pr)}</dd>
        <dt>Your permission</dt>
        <dd>
          {PERMISSION_WORDS[detail.permission]} on {pr.base.repo.owner}/{pr.base.repo.name}
        </dd>
        <dt>Review requested</dt>
        <dd>
          {pr.requestedReviewers.length === 0
            ? 'Nobody'
            : pr.requestedReviewers
                .map((r) => (r.team ? `${r.name} (team)` : r.name))
                .join(', ')}
        </dd>
        <dt>Checks</dt>
        <dd
          class:c-status--danger={detail.checksState === 'failing'}
          class:c-status--warn={detail.checksState === 'pending'}
          class:c-status--ok={detail.checksState === 'passing'}
        >
          {checksLine(pr.checks)}
        </dd>
        {#if detail.baseProtection}
          <dt>Base protection</dt>
          <dd>{protectionLine(detail.baseProtection)}</dd>
        {/if}
        <dt>Read as</dt>
        <dd>
          {detail.account.login ?? 'an unknown account'} on {detail.account.host}{detail
            .account.version
            ? `, gh ${detail.account.version}`
            : ''}
        </dd>
      </dl>

      <section
        class="c-work-state c-work-review__prepare"
        aria-labelledby="work-review-prepare"
      >
        <span class="c-work-state__eyebrow">
          {listed ? 'Review worktree ready' : 'No review worktree yet'}
        </span>
        <h2 class="c-work-state__title" id="work-review-prepare">
          {reusing ? 'Review worktree' : 'Prepare review worktree'}
        </h2>
        <p class="c-work-state__prose">
          {reusing
            ? 'This pull request already has a worktree, and it is used as it is.'
            : `Checks out the pull request's head commit, ${pr.head.sha.slice(0, 7)}, in a worktree of its own.`}
          Nothing is pushed, and nothing is posted to GitHub.
        </p>

        {#if !reusing}
          <fieldset class="c-work-review__options" disabled={preparing}>
            <legend class="c-work-review__legend">Check out</legend>
            <Choice
              type="radio"
              name="review-checkout"
              checked={checkout === 'detached'}
              onchange={(on) => on && (checkout = 'detached')}
            >
              Detached at {pr.head.sha.slice(0, 7)}
            </Choice>
            <Choice
              type="radio"
              name="review-checkout"
              checked={checkout === 'branch'}
              onchange={(on) => on && (checkout = 'branch')}
            >
              On a new local branch, <span class="u-mono">pr-{pr.number}</span>, with no
              upstream
            </Choice>
          </fieldset>
          <div class="c-work-review__options">
            <Choice
              checked={prepareEnvironment}
              disabled={preparing}
              onchange={(on) => (prepareEnvironment = on)}
            >
              Prepare environment
            </Choice>
            <p class="c-work-review__note">
              Runs this project's setup in the new worktree. Off unless you tick it: setup
              can clone databases or run migrations, and this is somebody else's branch.
            </p>
          </div>
        {/if}

        {#if previewError}
          <p class="c-work-review__error c-status--danger" role="alert">{previewError}</p>
        {:else if preview}
          <dl class="o-facts c-work-review__plan" aria-label="What Prepare will do">
            {#if preview.target.kind === 'reuse'}
              <dt>Worktree</dt>
              <dd>
                Uses <span class="u-mono">{preview.target.path}</span>, because {REUSE_WORDS[
                  preview.target.reason
                ]}.
                {preview.target.atHead
                  ? 'It is at the head.'
                  : 'It is behind the head and is not moved.'}
              </dd>
            {:else}
              <dt>Fetch</dt>
              <dd class="u-mono">
                {preview.fetch
                  ? `git fetch ${preview.fetch.remote} ${preview.fetch.refspec}`
                  : 'Nothing: the head commit is already here'}
              </dd>
              <dt>Create</dt>
              <dd class="u-mono">{preview.target.gitArgv.join(' ')}</dd>
            {/if}
            <dt>Setup</dt>
            <dd>
              {setupLine(preview.setup)}
              {#if preview.setup.kind === 'run'}
                <span class="c-work-review__argv u-mono"
                  >{preview.setup.argv.join(' ')}</span
                >
                <span class="c-work-review__note">in {preview.setup.cwd}</span>
              {/if}
            </dd>
          </dl>

          {#if preview.warnings.length > 0}
            <ul class="o-plain-list c-work-review__warnings">
              {#each preview.warnings as warning (warning)}
                <li class="c-status--warn">{warning}</li>
              {/each}
            </ul>
          {/if}
          {#if preview.preflight.length > 0}
            <PreflightList
              items={preview.preflight}
              {acknowledged}
              overrideLabel="Prepare anyway"
              disabled={preparing}
              onacknowledge={acknowledge}
            />
          {/if}
        {:else}
          <p class="c-work-state__note">Working out what Prepare will do…</p>
        {/if}

        <div class="c-work-state__actions">
          <!-- Nothing to do but open it once the worktree it would use is the one recorded. -->
          {#if !(reusing && listed && preview?.target.kind === 'reuse' && preview.target.path === listed.path)}
            <Button variant="accent" disabled={!ready || preparing} onclick={prepare}>
              {preparing
                ? 'Preparing…'
                : reusing
                  ? 'Use this worktree'
                  : 'Prepare review worktree'}
            </Button>
          {/if}
          {#if openable}
            <Button
              variant={reusing ? 'accent' : 'neutral'}
              onclick={() => onopenworktree(openable)}>Open in Worktrees</Button
            >
          {/if}
          {#if outcome?.kind === 'created'}
            <span class="c-work-state__note c-status--ok">Created at {outcome.path}.</span>
          {:else if outcome?.kind === 'reused'}
            <span class="c-work-state__note">Already there, at {outcome.path}.</span>
          {:else if outcome?.kind === 'setup_failed'}
            <span class="c-work-state__note c-status--danger"
              >The worktree is there, and setup failed. Open it to retry setup or read why.</span
            >
          {/if}
        </div>
        {#if prepareError}
          <p class="c-work-review__error c-status--danger" role="alert">{prepareError}</p>
        {/if}
      </section>
    {:else if !detailError}
      <p class="c-work-state__note">Reading the pull request…</p>
    {/if}
  </div>
{/if}
