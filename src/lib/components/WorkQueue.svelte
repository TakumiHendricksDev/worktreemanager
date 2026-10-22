<script lang="ts">
  /**
   * The Work view's left column: what there is to review, by where it came from.
   *
   * It takes the sidebar's place while Work is up, the way Home's tree does and for the same reason:
   * it lists the same project's worktrees, and two lists of them side by side would be one too many.
   * The sidebar stays mounted underneath, hidden, and comes back as it was.
   *
   * Two sections, the review slice's: My worktrees, from the project's listing, and Reviews, the
   * pull requests you added by URL or number. Adding one is a field at the foot of the section
   * rather than a dialog: it is one line of input, and its answer is a row appearing right above
   * it. The section offers no field at all for a project whose workflow names no forge, and says
   * what to configure instead, because wtm reads no pull request for such a project.
   *
   * # The sprint, for a project with a tracker
   *
   * A project whose workflow names a tracker gains the plan's sprint sections: Needs you, Running
   * and Ready to test, then Reviews, then Sprint, not started. An issue's section is its runs'
   * (`QueueSection::of` in Rust); its team status is a chip on the row, beside the run's words and
   * never in place of them. Above them, the sprint's name and goal, when it was synced, Sync, and
   * the List | Board switch, which gives the main column to the team's board. A worktree that
   * carries an issue's key is shown through its issue, so My worktrees lists the rest.
   *
   * Opening Work on a project syncs it, and so does coming back to the window (`App.svelte`) and
   * the Sync button. Nothing here reads the tracker on a timer.
   *
   * Rows are buttons in a list rather than a `role="tree"` like the sidebar's. There is nothing to
   * fold and no second level, so a tree's keyboard model would be ceremony with nothing behind it.
   */
  import { untrack } from 'svelte';

  import {
    myWorktrees,
    REVIEW_DOT,
    REVIEW_ROW_WORDS,
    reviewRows,
    reviewStatus,
    subjectOf,
  } from '../work';
  import { reviewRuns } from '../state/review-runs.svelte';
  import { reviews } from '../state/reviews.svelte';
  import { trackerQueue } from '../state/tracker.svelte';
  import { work } from '../state/work.svelte';
  import {
    bySection,
    CATEGORY_TONE,
    currentSprint,
    runWords,
    SECTION_DOT,
    sectionTitle,
    sprintEnds,
    syncedAt,
    type IssueRow,
    type QueueSection,
  } from '../tracker';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import TextInput from './ui/TextInput.svelte';

  const {
    visible,
    onnewworktree,
  }: {
    visible: boolean;
    /** New Worktree, which opens the create form in the main column. */
    onnewworktree: () => void;
  } = $props();

  /** The project's sprint queue, while it names a tracker. */
  const tracked = $derived(
    trackerQueue.projectId === workspace.activeProjectId && trackerQueue.queue?.tracker
      ? trackerQueue.queue
      : null,
  );
  const sprint = $derived(tracked ? currentSprint(tracked) : null);
  const sections = $derived(tracked ? bySection(tracked.issues) : null);
  /** Worktrees shown through the issue whose key they carry. */
  const viaIssues = $derived(
    new Set(tracked?.issues.flatMap((row) => row.worktrees) ?? []),
  );
  const rows = $derived(
    myWorktrees(workspace.worktrees).filter((row) => !viaIssues.has(row.id)),
  );
  const subject = $derived(subjectOf(rows, work.picked, workspace.selectedWorktreeId));
  const queue = $derived(
    reviews.projectId === workspace.activeProjectId ? reviews.queue : null,
  );
  const pullRequests = $derived(reviewRows(queue?.items ?? [], workspace.worktrees));

  let pasted = $state('');

  /** Each row's review status, from its newest run once read. */
  const statuses = $derived(
    Object.fromEntries(
      rows.map((row) => {
        const run = reviewRuns.latest[row.id];
        return [row.id, reviewStatus(run, run ? reviewRuns.staleness[run.flow.id] : null)];
      }),
    ),
  );

  // Each row's newest run, read once per row when Work shows the project. A run that moves after
  // that announces itself with `flow:changed`; nothing here polls.
  const rowIds = $derived(rows.map((row) => row.id).join('\n'));
  $effect(() => {
    const projectId = workspace.activeProjectId;
    void rowIds;
    if (!visible || !projectId) return;
    untrack(() => {
      for (const row of rows) {
        if (reviewRuns.latest[row.id] === undefined)
          void reviewRuns.load(projectId, row.id);
      }
    });
  });

  // Read on arrival and on a project switch, never on a timer: the queue is local, and each pull
  // request is read from the forge only when it is opened.
  $effect(() => {
    const projectId = workspace.activeProjectId;
    if (visible && projectId && workspace.activeProject?.usable)
      void reviews.load(projectId);
  });

  // The sprint queue: the mirror at once, then a sync, on arrival and on a project switch. The
  // store spaces syncs out, so showing Work twice in a row asks the tracker once.
  $effect(() => {
    const projectId = workspace.activeProjectId;
    if (!visible || !projectId || !workspace.activeProject?.usable) return;
    untrack(() => void trackerQueue.sync(projectId, 'open'));
  });

  // A worktree created or removed changes which issues have one: the mirror is read again for
  // that, with no tracker call.
  const worktreeIds = $derived(workspace.worktrees.map((w) => w.id).join('\n'));
  $effect(() => {
    const projectId = workspace.activeProjectId;
    void worktreeIds;
    if (!visible || !projectId) return;
    untrack(() => {
      if (trackerQueue.projectId === projectId && trackerQueue.queue?.tracker)
        void trackerQueue.load(projectId);
    });
  });

  async function addPullRequest(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const projectId = workspace.activeProjectId;
    const input = pasted.trim();
    if (!projectId || !input || reviews.adding) return;
    const id = await reviews.add(projectId, input);
    if (id !== null) {
      pasted = '';
      work.pickReview(id, 'overview');
    }
  }
</script>

{#snippet mineSection()}
  <section class="c-work-queue__section" aria-labelledby="work-queue-mine">
    <h3 class="c-work-queue__heading" id="work-queue-mine">
      {tracked ? 'Other worktrees' : 'My worktrees'}
      <span class="c-work-queue__count">{rows.length}</span>
    </h3>
    {#if rows.length > 0}
      <ul class="o-plain-list">
        {#each rows as row (row.id)}
          <li>
            <button
              class="c-work-row"
              class:is-selected={work.review === null &&
                work.issue === null &&
                row.id === subject?.id}
              aria-current={work.review === null &&
              work.issue === null &&
              row.id === subject?.id
                ? 'true'
                : undefined}
              title={row.detail}
              onclick={() => work.pick(row.id)}
            >
              <!-- The mark is one of `c-dot`'s states; the line beside it is what says the state,
                     so it is never in the shape alone. -->
              <span
                class="c-dot c-dot--{REVIEW_DOT[
                  statuses[row.id]?.status ?? 'unreviewed'
                ]} c-work-row__mark"
                aria-hidden="true"
              ></span>
              <span class="c-work-row__text">
                <span class="c-work-row__name">
                  {#if row.key}<span class="c-work-row__key">{row.key}</span
                    >{#if row.name}{' · '}{/if}{/if}{row.name}
                </span>
                <span class="c-work-row__status"
                  >{statuses[row.id]?.line ?? 'Not reviewed'}</span
                >
              </span>
            </button>
          </li>
        {/each}
      </ul>
    {:else if !workspace.loadingWorktrees}
      <p class="c-work-queue__empty">
        {workspace.activeProject
          ? 'No worktrees in this project yet.'
          : 'Add a repository to see its worktrees here.'}
      </p>
    {/if}
  </section>
{/snippet}

{#snippet reviewsSection()}
  <section class="c-work-queue__section" aria-labelledby="work-queue-reviews">
    <h3 class="c-work-queue__heading" id="work-queue-reviews">
      Reviews <span class="c-work-queue__count">{pullRequests.length}</span>
    </h3>
    {#if pullRequests.length > 0}
      <ul class="o-plain-list">
        {#each pullRequests as row (row.id)}
          {@const run = row.worktree ? reviewRuns.latest[row.worktree.id] : undefined}
          {@const reviewed = run
            ? reviewStatus(run, reviewRuns.staleness[run.flow.id])
            : null}
          <li>
            <button
              class="c-work-row"
              class:is-selected={work.review === row.id}
              aria-current={work.review === row.id ? 'true' : undefined}
              title={row.detail}
              onclick={() => work.pickReview(row.id, row.worktree ? 'reviews' : 'overview')}
            >
              {#if reviewed}
                <span
                  class="c-dot c-dot--{REVIEW_DOT[reviewed.status]} c-work-row__mark"
                  aria-hidden="true"
                ></span>
              {:else}
                <span
                  class="c-dot c-work-row__mark"
                  class:c-dot--idle={row.status === 'no_worktree'}
                  class:c-dot--done={row.status === 'worktree'}
                  aria-hidden="true"
                ></span>
              {/if}
              <span class="c-work-row__text">
                <span class="c-work-row__name">{row.name}</span>
                <span class="c-work-row__status"
                  >{reviewed ? reviewed.line : REVIEW_ROW_WORDS[row.status]}{row.reviewing
                    ? " · you're reviewing"
                    : ''}</span
                >
              </span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}

    {#if queue?.forge}
      {#if pullRequests.length === 0}
        <p class="c-work-queue__empty">
          Nothing to review yet. Add a pull request by its URL or number.
        </p>
      {/if}
      <form class="c-work-queue__add" onsubmit={addPullRequest}>
        <TextInput
          bind:value={pasted}
          mono
          placeholder="URL or #number"
          ariaLabel="Pull request URL or number"
          disabled={reviews.adding}
        />
        <Button type="submit" size="sm" disabled={reviews.adding || pasted.trim() === ''}>
          {reviews.adding ? 'Reading…' : 'Add'}
        </Button>
      </form>
      {#if reviews.addError}
        <p class="c-work-queue__error c-status--danger" role="alert">
          {reviews.addError}
        </p>
      {/if}
    {:else if queue}
      <p class="c-work-queue__empty">
        This project's workflow names no forge, so wtm reads no pull requests for it. Add
        <code>forge = "github"</code> to its <code>[workflow]</code> in your own config.
      </p>
    {/if}
    {#if reviews.queueError && reviews.projectId === workspace.activeProjectId}
      <p class="c-work-queue__error c-status--danger" role="alert">
        {reviews.queueError}
      </p>
    {/if}
  </section>
{/snippet}

{#snippet issueRow(row: IssueRow)}
  <li>
    <button
      class="c-work-row"
      class:is-selected={work.issue === row.issue.key}
      aria-current={work.issue === row.issue.key ? 'true' : undefined}
      title={row.issue.summary}
      onclick={() => work.pickIssue(row.issue.key)}
    >
      <span
        class="c-dot c-dot--{SECTION_DOT[row.section]} c-work-row__mark"
        aria-hidden="true"
      ></span>
      <span class="c-work-row__text">
        <span class="c-work-row__name">
          <span class="c-work-row__key">{row.issue.key}</span>{' · '}{row.issue.summary}
        </span>
        <span class="c-work-row__status">
          <!-- Team status and run status side by side: the tracker's word in its chip, wtm's after
               it. Neither is ever shown in the other's place. -->
          <span
            class="c-team-status c-team-status--{CATEGORY_TONE[row.issue.status.category]}"
            >{row.issue.status.name}</span
          >
          {runWords(row.run)}
        </span>
      </span>
    </button>
  </li>
{/snippet}

{#snippet issueSection(section: QueueSection, rowsIn: IssueRow[])}
  <section class="c-work-queue__section" aria-labelledby="work-queue-{section}">
    <h3 class="c-work-queue__heading" id="work-queue-{section}">
      {sectionTitle(section, sprint !== null)}
      <span class="c-work-queue__count">{rowsIn.length}</span>
    </h3>
    {#if rowsIn.length > 0}
      <ul class="o-plain-list">
        {#each rowsIn as row (row.issue.key)}
          {@render issueRow(row)}
        {/each}
      </ul>
    {/if}
  </section>
{/snippet}

<nav class="c-work-queue" class:is-hidden={!visible} aria-label="Work queue">
  <div class="c-work-queue__top">
    <h2 class="c-work-queue__title">Queue</h2>
    {#if tracked}
      <!-- The board takes the main column; the list stays here either way, so the queue's
           sections are one glance away from the columns. -->
      <div class="c-view-switch" role="group" aria-label="Queue layout">
        <button
          class="c-view-switch__option"
          class:is-selected={work.layout === 'list'}
          aria-pressed={work.layout === 'list'}
          onclick={() => (work.layout = 'list')}>List</button
        >
        <button
          class="c-view-switch__option"
          class:is-selected={work.layout === 'board'}
          aria-pressed={work.layout === 'board'}
          onclick={() => {
            work.layout = 'board';
            work.inboxOpen = false;
          }}>Board</button
        >
      </div>
    {/if}
  </div>

  {#if tracked}
    <section class="c-sprint-bar" aria-label="Sprint">
      {#if sprint}
        <p class="c-sprint-bar__name">{sprint.name}</p>
        <p class="c-sprint-bar__goal">
          {sprint.goal ? `Goal: ${sprint.goal}` : 'No sprint goal set'}
        </p>
      {:else if tracked.query}
        <p class="c-sprint-bar__name">
          {tracked.board === null ? 'Your query' : 'No active sprint'}
        </p>
      {/if}
      <div class="c-sprint-bar__meta">
        <span
          >{[sprint ? sprintEnds(sprint) : null, syncedAt(tracked.readAt)]
            .filter(Boolean)
            .join(' · ')}</span
        >
        {#if tracked.query && workspace.activeProjectId}
          {@const projectId = workspace.activeProjectId}
          <Button
            size="sm"
            variant="quiet"
            disabled={trackerQueue.syncing}
            title="Read the queue from the tracker now"
            onclick={() => void trackerQueue.sync(projectId, 'demand')}
            >{trackerQueue.syncing ? 'Syncing…' : 'Sync'}</Button
          >
        {/if}
      </div>
      {#if !tracked.query}
        <p class="c-sprint-bar__note">
          This project names a tracker but no queue. Add a <code>query</code> to its
          <code>[workflow.queue]</code> in your own config.
        </p>
      {:else if tracked.failure}
        <p class="c-sprint-bar__note c-status--danger" role="alert">
          The last sync failed: {tracked.failure.message}
        </p>
      {:else if tracked.outdated}
        <p class="c-sprint-bar__note">
          The queue's config changed since this was read. Sync to read it again.
        </p>
      {/if}
      {#if trackerQueue.error}
        <p class="c-sprint-bar__note c-status--danger" role="alert">{trackerQueue.error}</p>
      {/if}
    </section>
  {/if}

  <div class="c-work-queue__scroll">
    {#if tracked && sections}
      {@render issueSection('needs_you', sections.needs_you)}
      {@render issueSection('running', sections.running)}
      {@render issueSection('ready_to_test', sections.ready_to_test)}
      {@render reviewsSection()}
      {@render issueSection('not_started', sections.not_started)}
      {@render mineSection()}
    {:else}
      {@render mineSection()}
      {@render reviewsSection()}
    {/if}
  </div>

  <div class="c-work-queue__foot">
    <!-- Neutral where the sidebar's is accent: here the view is for reviewing, and its one
         primary action belongs to the workspace. Same label and gate as the sidebar's. -->
    <Button
      variant="neutral"
      full
      onclick={onnewworktree}
      disabled={!workspace.activeProject?.usable}
    >
      <Icon name="plus" size={14} /> New Worktree
    </Button>
  </div>
</nav>
