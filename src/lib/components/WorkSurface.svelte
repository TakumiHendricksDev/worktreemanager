<script lang="ts">
  /**
   * The Work view's main column: the workspace for the queue row picked, and the inbox beside it.
   *
   * Hidden rather than unmounted, like every surface, and mounted only once Work has been shown —
   * see `view.workMounted`.
   *
   * # What it draws, and what it does not yet
   *
   * For a worktree: the workspace header, Overview and Reviews. Overview is the worktree listing's
   * own facts. Reviews is `WorkReviewRun`: Run Review, then the run's state, its findings, its checks
   * and what its roles applied. The header's Run Review is the same action as the worktree bar's
   * (`run-review.ts`), and lands on Reviews. The plan's other tabs — Changes, Sessions, Activity —
   * come with the items that give them something to show.
   *
   * For a pull request picked in Reviews: `WorkReview`, with what the forge says about it and the
   * worktree a review of it reads.
   *
   * The inbox column is workflow runs' (`WorkRunInbox`), and the title bar's Inbox gives it the main
   * column as the focused Inbox (`WorkInbox`), with filters and room to answer a question. Approvals
   * from ordinary sessions stay where they are, in Home and on the panes; Home's Needs you lists the
   * runs' items beside them, so nothing a run asks is only in one place.
   *
   * It draws no session pane, which is why `attention.offScreen` counts every pane as off screen
   * while Work is up.
   */
  import { changesOf, myWorktrees, REVIEW_WORDS, rowTitle, subjectOf } from '../work';
  import { headline } from '../review-run';
  import { runReview } from '../run-review';
  import { reviewRuns } from '../state/review-runs.svelte';
  import { reviews } from '../state/reviews.svelte';
  import { work, type WorkTab } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';
  import WorkReview from './WorkReview.svelte';
  import WorkReviewRun from './WorkReviewRun.svelte';
  import WorkInbox from './WorkInbox.svelte';
  import WorkRunInbox from './WorkRunInbox.svelte';

  const {
    visible,
    onopenworktree,
  }: {
    visible: boolean;
    /** Leave Work for this worktree's sessions. */
    onopenworktree: (worktreeId: string) => void;
  } = $props();

  const TABS: { id: WorkTab; label: string }[] = [
    { id: 'overview', label: 'Overview' },
    { id: 'reviews', label: 'Reviews' },
  ];

  /** A pull request picked in Reviews, while it is still in this project's queue. */
  const reviewItem = $derived(
    work.review !== null && reviews.projectId === workspace.activeProjectId
      ? (reviews.queue?.items.find((item) => item.id === work.review) ?? null)
      : null,
  );
  const rows = $derived(myWorktrees(workspace.worktrees));
  const subject = $derived(subjectOf(rows, work.picked, workspace.selectedWorktreeId));
  const worktree = $derived(
    subject ? (workspace.worktrees.find((w) => w.id === subject.id) ?? null) : null,
  );
  /** The newest review run's state, which the header says instead of "Not reviewed" once there is one. */
  const runState = $derived.by(() => {
    const run = worktree ? reviewRuns.latest[worktree.id] : null;
    return run ? headline(run, reviewRuns.staleness[run.flow.id] ?? null) : null;
  });
  /** What the review would be of: the branch, which is what a reviewer names it by. */
  const reviewing = $derived(
    worktree?.branch ?? (worktree?.head ? worktree.head.slice(0, 7) : null),
  );

  let tabs = $state<HTMLElement | undefined>();

  /** Arrow-key roving across the strip, the way Settings' tabs move. */
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
    // This strip only: other `.c-tabs` may be mounted, hidden, elsewhere in the window.
    queueMicrotask(() =>
      tabs?.querySelector<HTMLElement>('[aria-selected="true"]')?.focus(),
    );
  }
</script>

<section class="c-work" class:is-hidden={!visible} aria-label="Work">
  {#if work.inboxOpen}
    <WorkInbox />
  {:else}
    <div class="c-work__columns">
      <div class="c-work__main">
        {#if reviewItem}
          <WorkReview
            item={reviewItem}
            visible={visible && !work.inboxOpen}
            {onopenworktree}
          />
        {:else if subject && worktree}
          <header class="c-work__header">
            <div class="c-work__heading">
              <h1 class="c-work__title" title={subject.detail}>{rowTitle(subject)}</h1>
              <!-- The issue key is not repeated here: the title leads with it, and Overview says
               where it came from. -->
              <p class="c-work__meta">
                <span class="u-mono">
                  {worktree.branch ??
                    `Detached${worktree.head ? ` at ${worktree.head.slice(0, 7)}` : ''}`}
                </span>
                <span>{changesOf(worktree)}</span>
              </p>
            </div>
            <div class="c-work__actions">
              <span class="c-badge"
                >{runState
                  ? `Run: ${runState.eyebrow.toLowerCase()}`
                  : REVIEW_WORDS[subject.review]}</span
              >
              <Button size="sm" onclick={() => onopenworktree(worktree.id)}
                >Open in Worktrees</Button
              >
              {#if workspace.activeProjectId}
                {@const projectId = workspace.activeProjectId}
                <Button
                  variant="accent"
                  size="sm"
                  ariaHaspopup="menu"
                  title="Review this worktree's committed and uncommitted work against its merge base"
                  onclick={(event) =>
                    void runReview(
                      event.currentTarget as Element,
                      projectId,
                      worktree.id,
                      () => {
                        work.tab = 'reviews';
                      },
                    )}>Run Review</Button
                >
              {/if}
            </div>
          </header>

          <div
            class="c-tabs c-tabs--inset"
            role="tablist"
            aria-label="Workspace"
            bind:this={tabs}
          >
            {#each TABS as tab (tab.id)}
              <button
                class="c-tabs__tab"
                class:is-active={work.tab === tab.id}
                role="tab"
                id="work-tab-{tab.id}"
                aria-selected={work.tab === tab.id}
                aria-controls="work-panel"
                tabindex={work.tab === tab.id ? 0 : -1}
                onclick={() => (work.tab = tab.id)}
                onkeydown={(event) => onTabKey(event, tab.id)}
              >
                {tab.label}
              </button>
            {/each}
          </div>

          <div
            class="c-work__panel"
            id="work-panel"
            role="tabpanel"
            aria-labelledby="work-tab-{work.tab}"
          >
            {#if work.tab === 'reviews' && workspace.activeProjectId}
              <WorkReviewRun
                visible={visible && !work.inboxOpen}
                projectId={workspace.activeProjectId}
                {worktree}
                reviewing={reviewing ?? rowTitle(subject)}
                subject={worktree.issueKey ?? worktree.branch}
              />
            {:else}
              <dl class="o-facts o-facts--wide">
                <dt>Branch</dt>
                <dd class="u-mono">{worktree.branch ?? 'Detached HEAD'}</dd>
                <dt>Head</dt>
                <dd class="u-mono">{worktree.head ? worktree.head.slice(0, 7) : '—'}</dd>
                <dt>Working tree</dt>
                <dd>{changesOf(worktree)}</dd>
                {#if worktree.ahead > 0 || worktree.behind > 0}
                  <dt>Upstream</dt>
                  <dd>{worktree.ahead} ahead, {worktree.behind} behind</dd>
                {/if}
                {#if worktree.issueKey}
                  <dt>Issue</dt>
                  <dd>{worktree.issueKey}, from the branch name</dd>
                {/if}
                <dt>Path</dt>
                <dd class="u-mono">{worktree.path}</dd>
              </dl>
            {/if}
          </div>
        {:else if !workspace.loadingWorktrees}
          <div class="c-placeholder">
            <p>
              {workspace.activeProject
                ? 'Pick a worktree in the queue.'
                : 'Add a repository to start reviewing its work.'}
            </p>
          </div>
        {/if}
      </div>

      <aside class="c-work__inbox" aria-labelledby="work-inbox-title">
        <WorkRunInbox />
      </aside>
    </div>
  {/if}
</section>
