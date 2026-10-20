<script lang="ts">
  /**
   * The Work view's main column: the workspace for the queue row picked, and the inbox beside it.
   *
   * Hidden rather than unmounted, like every surface, and mounted only once Work has been shown —
   * see `view.workMounted`.
   *
   * # What it draws, and what it does not yet
   *
   * The workspace header, Overview and Reviews. Overview is the worktree listing's own facts. Reviews
   * shows "Not reviewed yet", which is the one true state while nothing can run a review, and its
   * Run Review is disabled with the reason beside it rather than missing: it is where the view's
   * main action will be, and a button that appears later moves everything around it. The plan's
   * other tabs — Changes, Sessions, Activity — come with the items that give them something to show.
   *
   * The inbox is empty and says what will land in it. Approvals from ordinary sessions stay where
   * they are, in Home and on the panes: the plan gives this inbox review runs' questions and
   * failures, and moving the existing ones here is a separate decision.
   *
   * It draws no session pane, which is why `attention.offScreen` counts every pane as off screen
   * while Work is up.
   */
  import { changesOf, myWorktrees, REVIEW_WORDS, rowTitle, subjectOf } from '../work';
  import { work, type WorkTab } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import Button from './ui/Button.svelte';

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

  const rows = $derived(myWorktrees(workspace.worktrees));
  const subject = $derived(subjectOf(rows, work.picked, workspace.selectedWorktreeId));
  const worktree = $derived(
    subject ? (workspace.worktrees.find((w) => w.id === subject.id) ?? null) : null,
  );
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
  <div class="c-work__columns">
    <div class="c-work__main">
      {#if subject && worktree}
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
          <span class="c-badge">{REVIEW_WORDS[subject.review]}</span>
          <Button size="sm" onclick={() => onopenworktree(worktree.id)}
            >Open in Worktrees</Button
          >
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
          {#if work.tab === 'reviews'}
            <section class="c-work-state" aria-labelledby="work-review-title">
              <span class="c-work-state__eyebrow">Not reviewed yet</span>
              <h2 class="c-work-state__title" id="work-review-title">
                Review {reviewing ?? rowTitle(subject)}
              </h2>
              <p class="c-work-state__prose">
                A review reads a snapshot of this worktree, uncommitted work included,
                against its merge base. It needs no pull request and changes nothing in your
                tracker.
              </p>
              <div class="c-work-state__actions">
                <Button variant="accent" disabled>Run Review</Button>
                <span class="c-work-state__note"
                  >Review runs are not available in this build.</span
                >
              </div>
            </section>
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
      <h2 class="c-work__inbox-title" id="work-inbox-title">
        Inbox <span class="c-work__count">0</span>
      </h2>
      <p class="c-work__hint">
        Nothing needs you. Questions, approvals and failures from review runs land here, and
        stay until you resolve them.
      </p>
    </aside>
  </div>
</section>
