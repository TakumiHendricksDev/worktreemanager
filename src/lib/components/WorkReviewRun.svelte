<script lang="ts">
  /**
   * The Reviews tab for one worktree: Run Review before there is a review, and then the review —
   * its state, its findings grouped by class, the checks the app ran, and what the roles were
   * given to judge by.
   *
   * # Nine states, one block
   *
   * Not reviewed, running, waiting for usage capacity, interrupted, a role failed, stale, reviewed
   * (clean included, with any check that did not pass said so), can't start over billing, and
   * can't run for want of a skill. Each is the `c-work-state` block with its words from
   * `review-run.ts`, so the queue row, the header pill and this block never describe a run two
   * ways. A billing refusal is not a run — nothing was recorded — so it is drawn from the refused
   * action's error rather than from a run.
   *
   * # Stale is asked for, then drawn
   *
   * Whether the findings still hold is `review_staleness`, which asks git. It is asked when a run is
   * shown, when it settles, and when this worktree's listing changes — its head, or whether it has
   * changes — which the window learns on focus. Never on a timer.
   *
   * # The actions are the record's
   *
   * Dismiss, Save as rule and the reruns go through `reviewRuns` and back to Rust; Save as rule
   * opens R7's dialog on the stored finding, by its id. Open in Code goes through `codeRequests`,
   * which takes you to the worktree's Code tab at the line.
   *
   * # A teammate's pull request
   *
   * With `pullRequest`, the worktree is that pull request's review worktree and the block is the
   * pull request's review: Run Review compares with the pull request's base, the header says which
   * head was reviewed and which the pull request is at now, and when those differ the one action
   * is Re-review changes (Rerun affected would read the old head again). Heads the review moved on
   * from are listed with what became of each finding, and Finish review opens the outcome sheet.
   * Staleness is asked again whenever the window's reading of the pull request names a new head,
   * which it reads on focus.
   *
   * # Your own work
   *
   * Without `pullRequest`, a finished review offers Fix findings: a writer role fixes the findings
   * you pick, in this worktree, and wtm commits it on your branch (`WorkFixSheet`). Nothing leaves
   * the Mac and nothing needs approving, so the fix is simply ready, and the run offers to read what
   * changed and to review it again, which starts a new review of the worktree as it now stands. A
   * teammate's pull request fixes from its outcome sheet instead, and ends in a route.
   *
   * # Blocked on the environment
   *
   * A check that needs services which were not answering is drawn blocked, never passed or
   * skipped, with the services named; while anything is blocked the run carries `pendingStart`, and
   * the Start environment and run card (also in the inbox) is the one action. A runtime role's
   * screenshots are listed under Runtime, and a finding that cites one (`s12`) shows it.
   *
   * # Usage capacity
   *
   * A role waiting at its provider's limit is the capacity card (`WorkCapacity`, also in the inbox),
   * with its three choices. A role whose work came from another session — a fallback, or a role you
   * ran once elsewhere — shows where from, and the handover it was given is one click open under its
   * row. A role waiting for a run-session place says which cap is full on its own line.
   *
   * # Arriving from the inbox
   *
   * An inbox item or a notification about a role or a finding sets `work.focus`; once that target
   * is in the run shown, it is scrolled to and marked, a folded group opened for it. A ready review
   * on screen in a focused window has been read, so this tells the inbox (`inbox.seen`), which is
   * what settles a "review ready" item — and only that kind.
   */
  import { untrack } from 'svelte';

  import type {
    FindingRecord,
    PublicationView,
    PullRequest,
    ReviewItem,
    Worktree,
  } from '../ipc/types';
  import {
    canFinish,
    canRereview,
    currentOutcome,
    FATE_TONE,
    FATE_WORDS,
    movedTo,
    outcomeLine,
    short,
  } from '../pr-review';
  import { canFixFindings, fixToSend, isOwnReview, leaseLine } from '../local-fix';
  import {
    canResume,
    canRunAgain,
    canStop,
    canStopFix,
    CHECK_TONE,
    CHECK_WORDS,
    checkLines,
    findingGroups,
    GROUP_WORDS,
    headline,
    listedSteps,
    providerName,
    latestRound,
    queueLine,
    recipeProblem,
    resumeWords,
    ruleLabel,
    runsOn,
    servicesLine,
    skippedLine,
    STEP_TONE,
    stepLine,
    switchInto,
    switchLine,
  } from '../review-run';
  import { commands } from '../ipc/commands';
  import { draftOf } from '../publish';
  import { codeRequests } from '../state/code-request.svelte';
  import { inbox } from '../state/inbox.svelte';
  import { otherProvider } from '../recipes';
  import { reviewRuns } from '../state/review-runs.svelte';
  import { settingsRequest } from '../state/settings.svelte';
  import { work, type WorkFocus } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import CodeText from './CodeText.svelte';
  import RuleDialog from './RuleDialog.svelte';
  import Button from './ui/Button.svelte';
  import WorkCapacity from './WorkCapacity.svelte';
  import WorkFinding from './WorkFinding.svelte';
  import WorkFixSheet from './WorkFixSheet.svelte';
  import WorkPrOutcome from './WorkPrOutcome.svelte';
  import WorkPublication from './WorkPublication.svelte';
  import WorkRoute from './WorkRoute.svelte';
  import WorkShot from './WorkShot.svelte';
  import WorkStartApproval from './WorkStartApproval.svelte';

  const {
    visible,
    projectId,
    worktree,
    reviewing,
    subject,
    pullRequest = null,
  }: {
    /** Whether the tab is on screen: Work is up and its Inbox is not over it. */
    visible: boolean;
    projectId: string;
    worktree: Worktree;
    /** What the review is of, as its title says it: the branch, or a short head. */
    reviewing: string;
    /** The work item a rule saved from a finding names, such as an issue key or the branch. */
    subject: string | null;
    /**
     * A teammate's pull request whose review worktree this is: its queue item, and the window's
     * newest reading of it from the forge.
     */
    pullRequest?: { item: ReviewItem; read: PullRequest | null } | null;
  } = $props();

  const worktreeId = $derived(worktree.id);
  const run = $derived(reviewRuns.latest[worktreeId]);
  const stale = $derived(run ? (reviewRuns.staleness[run.flow.id] ?? null) : null);
  const recipes = $derived(reviewRuns.recipes[projectId] ?? null);
  const busy = $derived(reviewRuns.busy[worktreeId] ?? false);
  const error = $derived(reviewRuns.errors[worktreeId] ?? null);
  const problem = $derived(reviewRuns.problems[worktreeId] ?? null);
  const top = $derived(run ? headline(run, stale) : null);
  /** A role that could not run for want of a skill: panel 9's Edit recipe and Move role. */
  const blocked = $derived(run ? recipeProblem(run) : null);
  const groups = $derived(run ? findingGroups(run.findings) : []);
  const dismissed = $derived(run ? run.findings.filter((f) => f.dismissed !== null) : []);
  const checks = $derived(run ? checkLines(run, stale) : []);
  const prReview = $derived(run?.pullRequest ?? null);
  /** The pull request's new head, when the review read an older one. */
  const moved = $derived(prReview ? movedTo(prReview, pullRequest?.read ?? null) : null);
  const outcome = $derived(prReview ? currentOutcome(prReview) : null);
  /** The review's current publish, if it has been published or tried. */
  const publication = $derived(prReview?.publications.at(-1) ?? null);
  /** A local fix's routes, oldest first: each send as far as it got. */
  const routes = $derived(prReview?.routes ?? []);
  const lease = $derived(run ? leaseLine(run.leases) : null);
  let finishing = $state(false);
  /** The sheet for fixing your own work's findings, and which of its two moments is open. */
  let fixSheet = $state<'pick' | 'read' | null>(null);
  const showRerun = $derived(
    run !== undefined &&
      run !== null &&
      moved === null &&
      (stale?.changed === true ||
        run.findings.some(
          (f) => f.dismissed === null && stale?.staleFindings.includes(f.id),
        )) &&
      run.flow.state.end === null &&
      run.flow.state.hold === null &&
      !run.running &&
      run.flow.state.phase === 'consolidated',
  );

  let chosen = $state<string | null>(null);
  const recipe = $derived(chosen ?? run?.review.recipe ?? recipes?.[0]?.id ?? null);
  const chosenRecipe = $derived(
    recipes?.find((r) => r.id === recipe) ?? recipes?.[0] ?? null,
  );
  let saving = $state<FindingRecord | null>(null);

  // Read again whenever the worktree shown changes, by id rather than by object: a worktree
  // listing refresh hands over new objects for the same worktrees.
  $effect(() => {
    const project = projectId;
    const id = worktreeId;
    untrack(() => {
      void reviewRuns.load(project, id);
      void reviewRuns.loadRecipes(project);
    });
  });

  /** What the listing says about the worktree's contents: when it moves, staleness may have. */
  const treeKey = $derived(
    `${worktree.head ?? ''}:${worktree.dirty}:${worktree.staged}:${worktree.untracked}:${
      pullRequest?.read?.head.sha ?? ''
    }`,
  );
  const settledFlow = $derived(run && !run.running && run.snapshot ? run.flow.id : null);
  $effect(() => {
    const flow = settledFlow;
    void treeKey;
    if (flow !== null) untrack(() => void reviewRuns.checkStaleness(flow));
  });

  /** A settled run on screen has been read: the inbox's "review ready" for it is done. */
  $effect(() => {
    const flow = visible && run && !run.running ? run.flow.id : null;
    untrack(() => inbox.seen(flow));
    return () => untrack(() => inbox.seen(null));
  });

  /** The target the inbox brought you to, once it was found in this run and scrolled to. */
  let targeted = $state<WorkFocus | null>(null);
  let actedOn = 0;
  $effect(() => {
    const focus = work.focus;
    const shown = run;
    if (!visible || !focus || !shown || focus.seq === actedOn) return;
    const here =
      focus.kind === 'finding'
        ? shown.findings.some((f) => f.id === focus.id)
        : focus.kind === 'publication'
          ? (shown.pullRequest?.publications.some((p) => p.approval.id === focus.id) ??
              false) ||
            (shown.pullRequest?.routes.some((r) => r.approval.id === focus.id) ?? false)
          : shown.steps.some((s) => s.id === focus.id);
    // Not in this run yet, or ever: a newer run of the worktree, or one still loading. The next
    // read of the run tries again; a finding superseded since is simply not marked.
    if (!here) return;
    actedOn = focus.seq;
    targeted = focus;
    // Taken, so a later mount of this tab — after the Inbox was open over it — does not scroll
    // back to where an old click went. The mark stays until the next arrival.
    untrack(() => {
      work.focus = null;
    });
    const id =
      focus.kind === 'finding'
        ? `work-finding-${focus.id}`
        : focus.kind === 'publication'
          ? shown.pullRequest?.routes.some((r) => r.approval.id === focus.id)
            ? `work-route-${focus.id}`
            : `work-publication-${focus.id}`
          : `work-role-${focus.id}`;
    queueMicrotask(() => {
      const element = document.getElementById(id);
      if (!element) return;
      // A suggestion or a dismissed finding sits in a folded group; arriving at it opens it.
      const folded = element.closest('details');
      if (folded) folded.open = true;
      element.scrollIntoView({ block: 'center' });
      element.focus({ preventScroll: true });
    });
  });

  function isTargeted(kind: WorkFocus['kind'], id: number): boolean {
    return targeted?.kind === kind && targeted.id === id;
  }

  function start(): void {
    if (!recipe) return;
    if (pullRequest)
      // Starting may move the review worktree to the head, so the listing is read again after.
      void reviewRuns
        .startPullRequest(
          projectId,
          worktreeId,
          pullRequest.item.id,
          recipe,
          pullRequest.read?.head.sha ?? null,
        )
        .then(() => workspace.refreshWorktrees());
    else void reviewRuns.start(projectId, worktreeId, recipe);
  }

  /** True from the click until the run is back: the fetch and the checkout come first. */
  let following = $state(false);
  function rereview(): void {
    if (!run || !moved) return;
    following = true;
    void reviewRuns
      .rereview(projectId, worktreeId, run.flow.id, moved)
      .then(() => workspace.refreshWorktrees())
      .finally(() => (following = false));
  }

  function publicationAction(action: () => Promise<PublicationView>): void {
    if (!run) return;
    void reviewRuns.publication(worktreeId, run.flow.id, action);
  }

  /**
   * Re-review the head the pull request moved to, instead of publishing what was approved at the
   * old one: the stale publish is stopped first, so it does not wait in the inbox for a decision
   * already made.
   */
  async function rereviewInstead(stale: PublicationView): Promise<void> {
    await reviewRuns.publication(worktreeId, stale.approval.flow, () =>
      commands.cancelPublication(stale.approval.id),
    );
    rereview();
  }

  /** Approve exactly what was approved before, against the head it moved to. */
  async function publishAnyway(stale: PublicationView, head: string): Promise<void> {
    const hash = stale.approval.binding.payload;
    if (!run || !hash) return;
    const flow = run.flow.id;
    await reviewRuns.publication(worktreeId, flow, () =>
      reviewRuns.approvePublish(worktreeId, flow, draftOf(stale.review), hash, head),
    );
  }

  function open(path: string, line: number | null): void {
    codeRequests.open(projectId, worktreeId, path, line);
  }

  /** The command that signs a provider in to its subscription, as its CLI spells it. */
  function signIn(provider: string): string {
    return provider === 'codex' ? 'codex login' : 'claude /login';
  }

  /** Roles that only read, in the snapshot copy: the line the design ends the Applied list with. */
  const readOnly = $derived(
    run !== undefined &&
      run !== null &&
      run.steps.length > 0 &&
      run.steps.every((s) => s.profile === 'reviewer'),
  );
  const inSnapshot = $derived(
    run !== undefined && run !== null && run.steps.every((s) => s.workspace === 'snapshot'),
  );
</script>

{#snippet credits(message: string)}
  <section class="c-work-state c-work-state--danger" aria-labelledby="work-credits-title">
    <span class="c-work-state__eyebrow">Can't start: usage credits</span>
    <h2 class="c-work-state__title" id="work-credits-title">
      A role would bill usage credits it was not allowed to
    </h2>
    <p class="c-work-state__prose"><CodeText text={message} /></p>
    <p class="c-work-state__note">
      Headless Claude Code bills Fable to usage credits without asking, and a run session
      has nobody watching it. Nothing was started.
    </p>
    <div class="c-work-state__actions">
      <Button size="sm" disabled={busy || recipe === null} onclick={start}
        >Check again</Button
      >
    </div>
  </section>
{/snippet}

{#snippet billing(providers: string[], message: string)}
  <section class="c-work-state c-work-state--danger" aria-labelledby="work-billing-title">
    <span class="c-work-state__eyebrow">Can't start: billing</span>
    <h2 class="c-work-state__title" id="work-billing-title">
      {providers.length > 0
        ? `${providers.map(providerName).join(' and ')} would not bill your subscription`
        : 'A provider would not bill your subscription'}
    </h2>
    <p class="c-work-state__prose">
      <CodeText text={message} /> Review runs use subscriptions only, so nothing was started.
    </p>
    {#if providers.length > 0}
      <p class="c-work-state__note">
        Sign in with {#each providers as provider, index (provider)}{#if index > 0}{' or '}{/if}<code
            >{signIn(provider)}</code
          >{/each}, then check again.
      </p>
    {/if}
    <div class="c-work-state__actions">
      <Button size="sm" disabled={busy || recipe === null} onclick={start}
        >Check again</Button
      >
    </div>
  </section>
{/snippet}

{#if run && top}
  <div class="c-review">
    <p class="c-review__meta">
      <span>Recipe <strong>{chosenRecipe?.label ?? run.review.recipe}</strong></span>
      {#if run.snapshot}
        <span
          >Snapshot <code>{run.snapshot.headSha.slice(0, 7)}</code> with the working tree
          (tree
          <code>{run.snapshot.treeSha.slice(0, 7)}</code>)</span
        >
      {/if}
      <span>{listedSteps(run).filter((s) => s.status !== 'replaced').length} roles</span>
      {#if latestRound(run).length}<span>{latestRound(run).length}</span>{/if}
      <span
        >{new Date(latestRound(run).startedAt).toLocaleTimeString([], {
          hour: 'numeric',
          minute: '2-digit',
        })}</span
      >
      {#if pullRequest}
        <span
          >Compared with <code>{run.review.base}</code>, where #{pullRequest.item.number} would
          merge</span
        >
      {:else}
        <span>Compared with the <code>{run.review.base}</code> merge base</span>
      {/if}
    </p>

    {#if prReview && run.snapshot}
      <p class="c-review__meta c-review__heads">
        <span>Reviewed head <code>{short(run.snapshot.headSha)}</code></span>
        {#if moved}
          <span class="c-status--warn"
            >Current head <code>{short(moved)}</code> · new commits</span
          >
        {:else if prReview.mirror || pullRequest?.read}
          <span class="c-status--ok"
            >Current head <code>{short(run.snapshot.headSha)}</code> ✓</span
          >
        {/if}
        {#if prReview.reviewing}<span class="c-badge c-badge--accent"
            >You're reviewing this</span
          >{/if}
      </p>
    {/if}

    {#if problem?.kind === 'billing'}
      {@render billing(problem.providers, problem.message)}
    {:else if problem?.kind === 'credits' && !run.capacity}
      <!-- A refused "run once" is the capacity card's to say, beside the choice it refused. -->
      {@render credits(problem.message)}
    {/if}

    <section class="c-work-state c-work-state--{top.tone}" aria-labelledby="work-run-title">
      <span class="c-work-state__eyebrow">{top.eyebrow}</span>
      <h2 class="c-work-state__title" id="work-run-title"><CodeText text={top.title} /></h2>
      <p class="c-work-state__prose"><CodeText text={top.prose} /></p>

      <ul class="c-run-roles" aria-label="Roles">
        {#each listedSteps(run) as step (step.id)}
          <li
            class="c-run-roles__row"
            class:is-targeted={isTargeted('step', step.id)}
            id="work-role-{step.id}"
            tabindex="-1"
          >
            <span
              class="c-run-roles__mark c-run-roles__mark--{STEP_TONE[step.status]}"
              aria-hidden="true"
            ></span>
            <span class="c-run-roles__who">
              {step.role}{step.standsInFor ? ` (for ${step.standsInFor})` : ''} · {runsOn(
                step,
              )}
            </span>
            <span class="c-run-roles__line"><CodeText text={stepLine(run, step)} /></span>
            {#if step.handover}
              {@const came = switchInto(run, step)}
              <details class="c-handover">
                <summary class="c-handover__summary"
                  ><CodeText
                    text={came
                      ? `${switchLine(came)} · the handover it was given`
                      : 'The handover it was given'}
                  /></summary
                >
                <pre class="c-handover__text">{step.handover}</pre>
              </details>
            {/if}
          </li>
        {/each}
      </ul>
      {#if skippedLine(run)}
        <p class="c-work-state__note">{skippedLine(run)}</p>
      {/if}
      {#if queueLine(run)}
        <p class="c-work-state__note"><CodeText text={queueLine(run) ?? ''} /></p>
      {/if}

      <div class="c-work-state__actions">
        {#if showRerun && stale}
          <Button
            variant="accent"
            size="sm"
            disabled={busy || stale.affected.length === 0}
            title={stale.affected.length > 0
              ? `Runs ${stale.affected.join(', ')} against a new snapshot`
              : 'No role read a file that changed'}
            onclick={() => void reviewRuns.rerun(worktreeId, run.flow.id, 'affected')}
          >
            Rerun affected roles ({stale.affected.length})
          </Button>
          <Button
            size="sm"
            disabled={busy}
            onclick={() => void reviewRuns.rerun(worktreeId, run.flow.id, 'all')}
            >Rerun everything</Button
          >
        {/if}
        {#if canResume(run) && !run.capacity}
          <!-- Neutral beside panel 9's Edit recipe: retrying asks the same provider again, which
               helps only once the skill is installed for it. -->
          <Button
            variant={showRerun || blocked ? 'neutral' : 'accent'}
            size="sm"
            disabled={busy}
            onclick={() => void reviewRuns.resume(worktreeId, run.flow.id)}
          >
            {resumeWords(run)}
          </Button>
        {/if}
        {#if canStop(run)}
          <Button
            variant="quiet"
            size="sm"
            disabled={busy}
            onclick={() => void reviewRuns.stop(worktreeId, run.flow.id)}
            >Stop review</Button
          >
        {/if}
        {#if canStopFix(run)}
          <Button
            variant="quiet"
            size="sm"
            disabled={busy}
            onclick={() => void reviewRuns.cancelFix(worktreeId, run.flow.id)}
            >Stop fix</Button
          >
        {/if}
        {#if isOwnReview(run) && fixToSend(run)}
          <Button
            size="sm"
            variant="accent"
            disabled={busy}
            onclick={() => (fixSheet = 'read')}>Read what changed…</Button
          >
        {/if}
        {#if canFixFindings(run)}
          <Button
            variant={showRerun || blocked || fixToSend(run) ? 'neutral' : 'accent'}
            size="sm"
            disabled={busy}
            title="A writer role fixes the findings you pick, in this worktree, and wtm commits it on your branch"
            onclick={() => (fixSheet = 'pick')}>Fix findings…</Button
          >
        {/if}
        {#if canRereview(run, moved)}
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            title={moved
              ? `Moves the review worktree to ${short(moved)} and reruns the roles its changes affect`
              : undefined}
            onclick={rereview}>{following ? 'Re-reviewing…' : 'Re-review changes'}</Button
          >
        {/if}
        {#if canFinish(run)}
          <Button
            variant={showRerun || moved ? 'neutral' : 'accent'}
            size="sm"
            disabled={busy}
            onclick={() => (finishing = true)}
            >{fixToSend(run)
              ? 'Read the diff and choose where it goes…'
              : 'Finish review…'}</Button
          >
        {/if}
        {#if canRunAgain(run) && !showRerun && !moved && recipe}
          <Button size="sm" disabled={busy} onclick={start}
            >{isOwnReview(run) && fixToSend(run) ? 'Review the fix' : 'Run again'}</Button
          >
        {/if}
        {#if blocked}
          {@const stuck = blocked}
          <!-- Panel 9, "can't run: recipe": both open Settings › Recipes at the role. Moving it is a
               provider change in the role's editor, checked by Apply and written by Save. -->
          <Button
            variant="accent"
            size="sm"
            disabled={busy}
            onclick={() =>
              settingsRequest.showRecipes({
                project: projectId,
                recipe: run.review.recipe,
                role: stuck.role,
                provider: null,
                problem: stuck,
              })}>Edit recipe</Button
          >
          <Button
            size="sm"
            disabled={busy}
            onclick={() =>
              settingsRequest.showRecipes({
                project: projectId,
                recipe: run.review.recipe,
                role: stuck.role,
                provider: otherProvider(stuck.provider),
                problem: stuck,
              })}>Move role to {providerName(otherProvider(stuck.provider))}</Button
          >
        {/if}
      </div>
      {#if moved && canRereview(run, moved)}
        <p class="c-work-state__note">
          Re-review changes moves the review worktree to {short(moved)} and reruns only the roles
          whose files changed, asking each to verify its earlier findings were addressed. The
          results at {short(run.snapshot?.headSha ?? '')} are kept below.
        </p>
      {/if}
      {#if lease}
        <p class="c-work-state__note"><CodeText text={lease} /></p>
      {/if}
      {#if outcome}
        <p class="c-work-state__note">
          {outcomeLine(outcome, prReview?.reviewedHead ?? null)}{outcome.notes
            ? `: “${outcome.notes}”`
            : ''}
        </p>
      {/if}
      {#if error && problem?.kind !== 'billing' && problem?.kind !== 'credits' && !run.capacity}
        <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
      {/if}
    </section>

    {#if run.capacity}
      {@const capacity = run.capacity}
      <WorkCapacity
        {capacity}
        {busy}
        error={problem?.kind === 'credits' ? problem.message : error}
        onchoose={(choice) =>
          void reviewRuns.chooseCapacity(worktreeId, run.flow.id, capacity.step, choice)}
        ontry={() => void reviewRuns.resume(worktreeId, run.flow.id)}
      />
    {/if}

    {#if run.pendingStart}
      {@const start = run.pendingStart}
      <WorkStartApproval
        {start}
        {busy}
        error={null}
        onstart={() =>
          void reviewRuns.startEnvironment(worktreeId, run.flow.id, start.binding)}
        ondecline={() => void reviewRuns.declineEnvironment(worktreeId, run.flow.id)}
      />
    {/if}

    {#if publication}
      <WorkPublication
        view={publication}
        head={pullRequest?.read?.head.sha ?? prReview?.mirror?.pullRequest.head.sha ?? null}
        busy={reviewRuns.busy[`publication:${worktreeId}`] ?? false}
        error={reviewRuns.errors[`publication:${worktreeId}`] ?? null}
        onretry={() =>
          publicationAction(() => commands.retryPublication(publication.approval.id))}
        onadd={() =>
          publicationAction(() => commands.addToPendingReview(publication.approval.id))}
        onleave={() =>
          publicationAction(() => commands.cancelPublication(publication.approval.id))}
        onrereview={canRereview(run, moved)
          ? () => void rereviewInstead(publication)
          : null}
        onagainst={(head) => void publishAnyway(publication, head)}
        onopen={(url) => void commands.openUrl(url)}
      />
    {/if}

    {#each routes as route (route.approval.id)}
      <WorkRoute
        view={route}
        busy={reviewRuns.busy[`route:${worktreeId}`] ?? false}
        error={reviewRuns.errors[`route:${worktreeId}`] ?? null}
        onretry={() =>
          void reviewRuns.route(worktreeId, run.flow.id, () =>
            commands.retryFixRoute(route.approval.id),
          )}
        onleave={() =>
          void reviewRuns.route(worktreeId, run.flow.id, () =>
            commands.cancelFixRoute(route.approval.id),
          )}
        onopen={(url) => void commands.openUrl(url)}
      />
    {/each}

    {#each groups as section (section.group)}
      {#if section.group === 'suggestion'}
        <details class="c-findings" open={groups.length === 1}>
          <summary class="c-findings__heading"
            >{GROUP_WORDS[section.group]} · {section.findings.length}</summary
          >
          {#each section.findings as finding (finding.id)}
            <WorkFinding
              {finding}
              view={run}
              {busy}
              stale={stale?.staleFindings.includes(finding.id) ?? false}
              targeted={isTargeted('finding', finding.id)}
              onopen={open}
              onsaverule={(f) => (saving = f)}
              ondismiss={(f, reason) => void reviewRuns.dismiss(worktreeId, f.id, reason)}
            />
          {/each}
        </details>
      {:else}
        <section class="c-findings" aria-labelledby="work-findings-{section.group}">
          <h3 class="c-findings__heading" id="work-findings-{section.group}">
            {GROUP_WORDS[section.group]} · {section.findings.length}
          </h3>
          {#each section.findings as finding (finding.id)}
            <WorkFinding
              {finding}
              view={run}
              {busy}
              stale={stale?.staleFindings.includes(finding.id) ?? false}
              targeted={isTargeted('finding', finding.id)}
              onopen={open}
              onsaverule={(f) => (saving = f)}
              ondismiss={(f, reason) => void reviewRuns.dismiss(worktreeId, f.id, reason)}
            />
          {/each}
        </section>
      {/if}
    {/each}

    {#if dismissed.length > 0}
      <details class="c-findings">
        <summary class="c-findings__heading">Dismissed · {dismissed.length}</summary>
        {#each dismissed as finding (finding.id)}
          <WorkFinding
            {finding}
            view={run}
            {busy}
            stale={false}
            targeted={isTargeted('finding', finding.id)}
            onopen={open}
            onsaverule={(f) => (saving = f)}
            ondismiss={(f, reason) => void reviewRuns.dismiss(worktreeId, f.id, reason)}
          />
        {/each}
      </details>
    {/if}

    {#if prReview && prReview.earlier.length > 0}
      <details class="c-findings c-review-earlier">
        <summary class="c-findings__heading">
          Earlier heads · {prReview.earlier.length}
        </summary>
        {#each [...prReview.earlier].reverse() as left (left.head.id)}
          <section
            class="c-review-earlier__head"
            aria-labelledby="work-earlier-{left.head.id}"
          >
            <h4 class="c-review-earlier__title" id="work-earlier-{left.head.id}">
              <code>{short(left.head.headSha)}</code>, until the author pushed
              <code>{short(left.head.nextHead)}</code>
              · {left.findings.length === 1
                ? '1 finding'
                : `${left.findings.length} findings`}
              {#if left.checks.length > 0}· {left.checks
                  .map((c) => `${c.check} ${c.status.replace('_', ' ')}`)
                  .join(', ')}{/if}
            </h4>
            {#if left.findings.length > 0}
              <ul class="o-plain-list c-review-earlier__list">
                {#each left.findings as earlier (earlier.id)}
                  <li class="c-review-earlier__item">
                    <span
                      class="c-review-earlier__fate c-review-earlier__fate--{FATE_TONE[
                        earlier.fate
                      ]}">{FATE_WORDS[earlier.fate]}</span
                    >
                    <span class="c-review-earlier__what"
                      ><CodeText text={earlier.title} /></span
                    >
                    {#if earlier.location}<code class="c-review-earlier__where"
                        >{earlier.location.path}:{earlier.location.startLine}</code
                      >{/if}
                  </li>
                {/each}
              </ul>
            {/if}
            {#each prReview.outcomes.filter((o) => o.headSha === left.head.headSha) as kept (kept.id)}
              <p class="c-review-earlier__kept">
                Kept private here{kept.notes ? `: “${kept.notes}”` : ''}
              </p>
            {/each}
          </section>
        {/each}
      </details>
    {/if}

    {#if checks.length > 0}
      <section class="c-review-checks" aria-labelledby="work-checks-title">
        <h3 class="c-findings__heading" id="work-checks-title">
          Checks · run by wtm, never on an agent's say-so
        </h3>
        <ul class="c-review-checks__list">
          {#each checks as check (check.id)}
            <li
              class="c-review-checks__item c-review-checks__item--{CHECK_TONE[check.state]}"
            >
              <span class="c-review-checks__mark" aria-hidden="true"></span>
              <span
                >{check.label} · {CHECK_WORDS[check.state]}{#if check.detail}: <CodeText
                    text={check.detail}
                  />{/if}{#if servicesLine(check.run)}, <CodeText
                    text={servicesLine(check.run) ?? ''}
                  />{/if}</span
              >
              {#if check.stale}<span class="c-finding__stale">Stale</span>{/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if run.screenshots.length > 0}
      <section class="c-review-runtime" aria-labelledby="work-runtime-title">
        <h3 class="c-findings__heading" id="work-runtime-title">
          Runtime · what the running app showed, in a private browser store
        </h3>
        <div class="c-review-runtime__shots">
          {#each run.screenshots as shot (shot.id)}
            <WorkShot {shot} />
          {/each}
        </div>
      </section>
    {/if}

    <p class="c-review__applied">
      Applied:
      {#if run.applied.rules.length > 0}
        rules {run.applied.rules.map((r) => ruleLabel(r.rule, r.revision)).join(', ')}
      {:else}
        no rules
      {/if}
      · {run.applied.sources.length > 0
        ? `instruction sources ${run.applied.sources.join(', ')}`
        : 'no instruction sources'}
      {#if run.applied.skills.length > 0}
        · {run.applied.skills
          .map((s) => `skill /${s.skill} (${providerName(s.provider)})`)
          .join(', ')}
      {/if}
      {#if readOnly}
        · roles ran read-only{inSnapshot ? ' in a snapshot copy' : ''}; none can push, merge
        or reach GitHub
      {/if}
    </p>
  </div>
{:else if problem?.kind === 'billing'}
  {@render billing(problem.providers, problem.message)}
{:else if problem?.kind === 'credits'}
  {@render credits(problem.message)}
{:else}
  <section class="c-work-state" aria-labelledby="work-review-title">
    <span class="c-work-state__eyebrow">Not reviewed yet</span>
    <h2 class="c-work-state__title" id="work-review-title">Review {reviewing}</h2>
    {#if pullRequest}
      <p class="c-work-state__prose">
        A review reads the pull request's head{pullRequest.read
          ? `, ${short(pullRequest.read.head.sha)},`
          : ''} in its review worktree, against
        <code
          >{pullRequest.read
            ? `${pullRequest.read.base.repo.owner}/${pullRequest.read.base.repo.name} ${pullRequest.read.base.branch}`
            : 'its base'}</code
        >, where it would merge. Roles only read; nothing is posted to GitHub.
      </p>
    {:else}
      <p class="c-work-state__prose">
        A review reads a snapshot of this worktree, committed and uncommitted work alike,
        against its merge base. It needs no pull request and changes nothing in your
        tracker.
      </p>
    {/if}
    {#if recipes && recipes.length > 1}
      <label class="c-work-state__field">
        Recipe
        <!-- The value shown is the recipe Run Review would use, the first until one is picked;
             binding `chosen` alone would show a blank select over a default that still applies. -->
        <select
          class="c-select"
          value={recipe}
          onchange={(event) => (chosen = event.currentTarget.value)}
        >
          {#each recipes as choice (choice.id)}
            <option value={choice.id}>{choice.label} · {choice.roles.length} roles</option>
          {/each}
        </select>
      </label>
    {/if}
    <div class="c-work-state__actions">
      <Button variant="accent" disabled={busy || recipe === null} onclick={start}
        >Run Review</Button
      >
      {#if recipes !== null && recipes.length === 0}
        <span class="c-work-state__note"
          >No review recipes yet: add <code>[workflow]</code> roles and a recipe to your own config.</span
        >
      {:else if chosenRecipe}
        <span class="c-work-state__note">
          {chosenRecipe.roles.map((role) => `${role.id} on ${runsOn(role)}`).join(' · ')}
        </span>
      {/if}
    </div>
    {#if error}
      <p class="c-work-state__error" role="alert"><CodeText text={error} /></p>
    {/if}
  </section>
{/if}

{#if finishing && run}
  <WorkPrOutcome
    view={run}
    read={pullRequest?.read ?? null}
    {busy}
    {error}
    onclose={() => (finishing = false)}
    onkeep={(notes) => reviewRuns.keepPrivate(worktreeId, run.flow.id, notes)}
    onpreview={(draft) => reviewRuns.previewPublish(run.flow.id, draft)}
    onapprove={(draft, hash, againstHead) =>
      reviewRuns.approvePublish(worktreeId, run.flow.id, draft, hash, againstHead)}
    onrereview={(head) =>
      void reviewRuns
        .rereview(projectId, worktreeId, run.flow.id, head)
        .then(() => workspace.refreshWorktrees())}
    onstartfix={(request) => reviewRuns.startFix(worktreeId, run.flow.id, request)}
    onfixdiff={() => reviewRuns.fixDiff(run.flow.id)}
    onroutes={() => reviewRuns.fixRoutes(run.flow.id)}
    onpreviewroute={(request) => reviewRuns.previewRoute(run.flow.id, request)}
    onapproveroute={(request, hash) =>
      reviewRuns.approveRoute(worktreeId, run.flow.id, request, hash)}
  />
{/if}

{#if fixSheet && run}
  <WorkFixSheet
    view={run}
    mode={fixSheet}
    onclose={() => (fixSheet = null)}
    onstart={(request) => reviewRuns.startFix(worktreeId, run.flow.id, request)}
    ondiff={() => reviewRuns.fixDiff(run.flow.id)}
    onreview={start}
  />
{/if}

{#if saving}
  <RuleDialog
    {projectId}
    projectName={workspace.activeProject?.name ?? ''}
    mode={{ kind: 'finding', finding: saving, subject }}
    onclose={() => (saving = null)}
    onsaved={() => (saving = null)}
  />
{/if}
