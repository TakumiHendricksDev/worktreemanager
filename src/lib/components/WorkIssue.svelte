<script lang="ts">
  /**
   * A sprint issue's workspace: what the tracker says about it, and what wtm is doing about it.
   *
   * # Two statuses, two places
   *
   * The header carries the team status as the tracker's own chip, and the run status as wtm's
   * badge, side by side and labelled, because they answer different questions: where the team
   * thinks the issue is, and whether a run needs you. The team status is not a control yet. Moving
   * an issue is a write, and the first item allowed to write to a tracker is Start Work.
   *
   * # Read on opening
   *
   * Opening an issue is an action, so it is read whole from the tracker then: its description,
   * parent, links and the project's mapped fields (`trackerQueue.open`). What was kept from the
   * last opening shows at once while that read is under way. The queue's sync never reads issues
   * one by one.
   *
   * # Its worktrees
   *
   * The worktrees whose branch or directory carries the key, found by the same rule the worktree
   * list uses (`extract_issue_key`). Their newest runs decided the issue's queue section; the
   * Reviews tab shows the first one's. Starting work on an issue that has none is Start Work's, a
   * later item, and the Overview says so rather than offering a button that does something else.
   */
  import { untrack } from 'svelte';

  import { runReview } from '../run-review';
  import { trackerQueue } from '../state/tracker.svelte';
  import { work, type WorkTab } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import {
    CATEGORY_TONE,
    ROLE_WORDS,
    runWords,
    syncedAt,
    TRACKER_NAMES,
    type IssueRow,
  } from '../tracker';
  import { worktreeLabel } from '../worktree-label';
  import Button from './ui/Button.svelte';
  import WorkReviewRun from './WorkReviewRun.svelte';

  const {
    issueKey,
    row,
    visible,
    onopenworktree,
  }: {
    issueKey: string;
    /** The issue's queue row, while the queue holds it. */
    row: IssueRow | null;
    visible: boolean;
    onopenworktree: (worktreeId: string) => void;
  } = $props();

  const TABS: { id: WorkTab; label: string }[] = [
    { id: 'overview', label: 'Overview' },
    { id: 'reviews', label: 'Reviews' },
  ];

  const view = $derived(trackerQueue.details[issueKey] ?? null);
  const detail = $derived(view?.detail ?? null);
  /** The newest word on the issue: the whole read when there is one, else the queue's row. */
  const issue = $derived(detail?.issue ?? row?.issue ?? null);
  const role = $derived(view?.role ?? row?.role ?? null);
  const trackerName = $derived(TRACKER_NAMES[view?.tracker ?? row?.tracker ?? 'jira']);
  const worktrees = $derived(
    (row?.worktrees ?? [])
      .map((id) => workspace.worktrees.find((w) => w.id === id))
      .filter((w) => w !== undefined),
  );
  /** The worktree the Reviews tab reads: the one whose run placed the issue, else the first. */
  const reviewed = $derived(
    worktrees.find((w) => w.id === row?.run?.worktree) ?? worktrees[0] ?? null,
  );
  const sprint = $derived.by(() => {
    const id = row?.sprint;
    const fromQueue = trackerQueue.queue?.sprints.find((s) => s.id === id);
    return fromQueue ?? detail?.sprints.find((s) => s.state === 'active') ?? null;
  });

  // Read whole on opening, and again when a different issue is opened. `issueKey` is a string, so
  // a queue reload handing over a new row object does not read it again. Untracked, because `open`
  // reads and writes the store's own state: tracked, each read it finished would start another,
  // which against a real tracker is one `acli` process after another for as long as the issue is
  // open.
  $effect(() => {
    const projectId = workspace.activeProjectId;
    const key = issueKey;
    if (visible && projectId) untrack(() => void trackerQueue.open(projectId, key));
  });

  let tabs = $state<HTMLElement | undefined>();

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
</script>

<header class="c-work__header">
  <div class="c-work__heading">
    <h1 class="c-work__title" title={issue?.summary ?? issueKey}>
      {issueKey}{issue ? ` · ${issue.summary}` : ''}
    </h1>
    <p class="c-work__meta">
      <span>
        {[
          issue?.kind,
          issue?.priority ? `${issue.priority} priority` : null,
          issue?.assignee ?? 'Unassigned',
          sprint?.name,
        ]
          .filter(Boolean)
          .join(' · ')}
      </span>
    </p>
  </div>
  <div class="c-work__actions">
    {#if issue}
      <span
        class="c-team-status c-team-status--{CATEGORY_TONE[issue.status.category]}"
        title="Team status, from {trackerName}">{issue.status.name}</span
      >
    {/if}
    <span class="c-badge" title="What wtm is doing about it"
      >{runWords(row?.run ?? null)}</span
    >
    {#if reviewed && workspace.activeProjectId}
      {@const projectId = workspace.activeProjectId}
      {@const worktreeId = reviewed.id}
      <Button size="sm" onclick={() => onopenworktree(worktreeId)}>Open in Worktrees</Button
      >
      <Button
        variant="accent"
        size="sm"
        ariaHaspopup="menu"
        title="Review this worktree's committed and uncommitted work against its merge base"
        onclick={(event) =>
          void runReview(event.currentTarget as Element, projectId, worktreeId, () => {
            work.tab = 'reviews';
            // The app just acted on this issue: read the queue again, so a status someone changed
            // meanwhile is what the queue shows beside the run.
            void trackerQueue.sync(projectId, 'after_action');
          })}>Run Review</Button
      >
    {/if}
  </div>
</header>

<div class="c-tabs c-tabs--inset" role="tablist" aria-label="Issue" bind:this={tabs}>
  {#each TABS as tab (tab.id)}
    <button
      class="c-tabs__tab"
      class:is-active={work.tab === tab.id}
      role="tab"
      id="issue-tab-{tab.id}"
      aria-selected={work.tab === tab.id}
      aria-controls="issue-panel"
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
  id="issue-panel"
  role="tabpanel"
  aria-labelledby="issue-tab-{work.tab}"
>
  {#if work.tab === 'reviews'}
    {#if reviewed && workspace.activeProjectId}
      <WorkReviewRun
        {visible}
        projectId={workspace.activeProjectId}
        worktree={reviewed}
        reviewing={reviewed.branch ?? worktreeLabel(reviewed).name}
        subject={issueKey}
      />
    {:else}
      <div class="c-placeholder">
        <p>
          No worktree carries {issueKey} in its branch or folder name, so there is nothing to
          review yet.
        </p>
      </div>
    {/if}
  {:else}
    {#if trackerQueue.detailErrors[issueKey]}
      <p class="c-issue__error c-status--danger" role="alert">
        Could not read {issueKey} from {trackerName}: {trackerQueue.detailErrors[issueKey]}
      </p>
    {/if}
    <dl class="o-facts o-facts--wide">
      <dt>Team status</dt>
      <dd>
        {#if issue}
          {issue.status.name}{role ? ` · ${ROLE_WORDS[role]}` : ' · not mapped'}
        {:else}
          —
        {/if}
        <span class="c-issue__source"
          >from {trackerName}, {trackerQueue.reading.includes(issueKey)
            ? 'reading now…'
            : syncedAt(view?.readAt ?? null).toLowerCase()}</span
        >
      </dd>
      <dt>Run</dt>
      <dd>{runWords(row?.run ?? null)}</dd>
      <dt>Worktrees</dt>
      <dd>
        {#if worktrees.length > 0}
          <ul class="o-plain-list c-issue__list">
            {#each worktrees as worktree (worktree.id)}
              <li>
                <span class="u-mono">{worktree.branch ?? worktreeLabel(worktree).name}</span
                >
                <Button size="sm" variant="link" onclick={() => onopenworktree(worktree.id)}
                  >Open</Button
                >
              </li>
            {/each}
          </ul>
        {:else}
          None carries {issueKey} in its branch or folder name. Starting work from an issue comes
          with a later version.
        {/if}
      </dd>
      {#if sprint}
        <dt>Sprint</dt>
        <dd>{sprint.name}{sprint.goal ? ` · Goal: ${sprint.goal}` : ''}</dd>
      {/if}
      {#if detail?.parent}
        <dt>Parent</dt>
        <dd>
          <span class="u-mono">{detail.parent.key}</span>{detail.parent.summary
            ? ` · ${detail.parent.summary}`
            : ''}
        </dd>
      {/if}
      {#if detail && detail.links.length > 0}
        <dt>Links</dt>
        <dd>
          <ul class="o-plain-list c-issue__list">
            {#each detail.links as link (link.relation + link.key)}
              <li>
                {link.relation}
                <span class="u-mono">{link.key}</span>{link.summary
                  ? ` · ${link.summary}`
                  : ''}
                {#if link.status}
                  <span
                    class="c-team-status c-team-status--{CATEGORY_TONE[
                      link.status.category
                    ]}">{link.status.name}</span
                  >
                {/if}
              </li>
            {/each}
          </ul>
        </dd>
      {/if}
      {#each detail?.fields ?? [] as field (field.id)}
        <dt>{field.name}</dt>
        <dd>{field.value ?? '—'}</dd>
      {/each}
      {#if issue && issue.labels.length > 0}
        <dt>Labels</dt>
        <dd>{issue.labels.join(', ')}</dd>
      {/if}
    </dl>
    {#if detail?.description}
      <h2 class="c-issue__heading">Description</h2>
      <p class="c-issue__description">{detail.description}</p>
    {/if}
  {/if}
</div>
