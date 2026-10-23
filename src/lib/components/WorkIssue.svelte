<script lang="ts">
  /**
   * A sprint issue's workspace: what the tracker says about it, and what wtm is doing about it.
   *
   * # Two statuses, two places
   *
   * The header carries the team status as the tracker's own chip, and the run status as wtm's
   * badge, side by side and labelled, because they answer different questions: where the team
   * thinks the issue is, and whether a run needs you. The team status is not a control: it moves
   * only from Start, which says so on its button before you press it, or from a drop on the board.
   *
   * # Start Work
   *
   * Start (or ⌘↵ here and on the queue's row) starts the issue's run, or shows the one it has. Where
   * its worktree comes from is resolution's (`flow/start.rs`), and a choice opens the Start sheet.
   * The run then makes or adopts its worktree. Overview's Run section is where everything about the
   * start is: its worktree and setup terminal, a crash's Resume, a pause, and each move of the team
   * status with what it read, including the race a tracker leaves open.
   *
   * # Planning and design
   *
   * Once the worktree is ready the project's planner starts by itself, and the Plan tab is where it
   * is followed and decided on (`WorkPlan`): its questions, its plan, your approval, and what it was
   * given. The Run section says where planning stands and leads there. A plan that needs a design
   * goes on to the Design tab (`WorkDesign`): the designer's revisions, your comments, your
   * approval, and the gate in front of implementing.
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
   * The worktree Start bound it to, then those whose branch or directory carries the key, found by
   * the same rule the worktree list uses (`extract_issue_key`). The Reviews tab shows the first.
   */
  import { untrack } from 'svelte';

  import { runReview } from '../run-review';
  import { overlay } from '../state/overlay.svelte';
  import { startWork } from '../state/start-work.svelte';
  import { trackerQueue } from '../state/tracker.svelte';
  import { work, type WorkTab } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import {
    CATEGORY_TONE,
    ROLE_WORDS,
    runWords,
    sameStatus,
    startColumn,
    syncedAt,
    TRACKER_NAMES,
    type IssueRow,
  } from '../tracker';
  import {
    CAUSE_WORDS,
    JOB_WORDS,
    moveCaption,
    moveWords,
    raceNote,
    STAY_WORDS,
  } from '../start-work';
  import { worktreeLabel } from '../worktree-label';
  import { planRunWords } from '../plan';
  import { codeRequests } from '../state/code-request.svelte';
  import { plan } from '../state/plan.svelte';
  import Terminal from './Terminal.svelte';
  import Button from './ui/Button.svelte';
  import WorkDesign from './WorkDesign.svelte';
  import WorkPlan from './WorkPlan.svelte';
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
    { id: 'plan', label: 'Plan' },
    { id: 'design', label: 'Design' },
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

  /** The issue sits in the board's start column: the status Start moves an issue to. */
  const startsHere = $derived.by(() => {
    const start = trackerQueue.queue ? startColumn(trackerQueue.queue) : null;
    return issue !== null && start !== null && sameStatus(issue.status.name, start);
  });
  /** Start Work's record of the issue: its run, job, moves and inbox. */
  const started = $derived(startWork.views[issueKey] ?? null);
  const job = $derived(started?.job ?? null);
  const flow = $derived(started?.flow ?? null);
  const open = $derived(flow !== null && flow.state.end === null);
  const starting = $derived(startWork.busy[issueKey] ?? false);
  const startError = $derived(startWork.errors[issueKey] ?? null);
  /** What Start does to the team status, for its button's caption. */
  const caption = $derived(started ? moveCaption(started.startMove, trackerName) : null);
  /** The bound worktree, while the listing has it. */
  const boundWorktree = $derived(
    job ? (workspace.worktrees.find((w) => w.id === job.worktree) ?? null) : null,
  );
  /** The run's planning, as the Plan tab reads it. */
  const planned = $derived(plan.views[issueKey] ?? null);
  const waitingQuestions = $derived(
    planned?.questions.filter((q) => q.question.answer === null).length ?? 0,
  );

  /** Open a file a question points at, in the issue's worktree. */
  function openFile(path: string, line: number | null): void {
    const projectId = workspace.activeProjectId;
    const worktree = job?.worktree ?? reviewed?.id;
    if (projectId && worktree) codeRequests.open(projectId, worktree, path, line);
  }

  function start(): void {
    const projectId = workspace.activeProjectId;
    if (projectId) void startWork.start(projectId, issueKey);
  }

  // ⌘↵ starts the issue shown, or shows its run: the queue's chord, here too while this
  // workspace is on screen and nothing else holds the keyboard.
  function onKey(event: KeyboardEvent): void {
    // Taken already by the queue's row, whose own ⌘↵ this is, when it had the focus.
    if (event.defaultPrevented) return;
    if (!visible || overlay.modals > 0 || event.key !== 'Enter') return;
    if (!(event.metaKey || event.ctrlKey)) return;
    const target = event.target as HTMLElement | null;
    if (target?.closest('input, textarea, [contenteditable="true"], #terminal-dock'))
      return;
    event.preventDefault();
    start();
  }

  // Start Work's record and the run's planning are read with the issue, untracked for the reason
  // the read below gives.
  $effect(() => {
    const projectId = workspace.activeProjectId;
    const key = issueKey;
    if (visible && projectId)
      untrack(() => {
        void startWork.load(projectId, key);
        void plan.load(projectId, key);
      });
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
    {#if issue && workspace.activeProjectId}
      <span class="c-issue-start">
        <Button
          variant={open ? 'neutral' : 'accent'}
          size="sm"
          disabled={starting}
          title={open
            ? 'Show its run (⌘↵)'
            : `Start its run and give it a worktree (⌘↵)${caption ? `. ${caption}.` : ''}`}
          onclick={start}
          >{starting ? 'Starting…' : open ? 'Go to run' : 'Start work'}</Button
        >
        {#if !open && caption}
          <span class="c-issue-start__caption">{caption}</span>
        {/if}
      </span>
    {/if}
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

<svelte:window onkeydown={onKey} />

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
  {#if work.tab === 'plan'}
    {#if workspace.activeProjectId}
      <WorkPlan
        projectId={workspace.activeProjectId}
        {issueKey}
        {visible}
        onopenfile={openFile}
      />
    {/if}
  {:else if work.tab === 'design'}
    {#if workspace.activeProjectId}
      <WorkDesign
        projectId={workspace.activeProjectId}
        {issueKey}
        {visible}
        {onopenworktree}
      />
    {/if}
  {:else if work.tab === 'reviews'}
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
    {#if startError}
      <p class="c-issue__error c-status--danger" role="alert">{startError}</p>
    {/if}
    {#if flow && job && workspace.activeProjectId}
      {@const projectId = workspace.activeProjectId}
      <section class="c-issue-run" aria-labelledby="issue-run-title">
        <header class="c-issue-run__head">
          <h2 class="c-issue-run__title" id="issue-run-title">Run</h2>
          <span class="c-issue-run__state"
            >{flow.state.end
              ? runWords(row?.run ?? null)
              : flow.state.hold?.kind === 'paused'
                ? 'Paused'
                : job.state === 'ready' && planned?.planning
                  ? planRunWords(planned)
                  : flow.state.hold?.kind === 'interrupted'
                    ? JOB_WORDS.interrupted
                    : JOB_WORDS[job.state]}</span
          >
          {#if open}
            <span class="c-issue-run__actions">
              {#if flow.state.hold?.kind === 'paused'}
                <Button
                  size="sm"
                  disabled={starting}
                  onclick={() => void startWork.unpause(projectId, issueKey, flow.id)}
                  >Resume run</Button
                >
              {:else if flow.state.hold === null && !planned?.running}
                <Button
                  size="sm"
                  variant="quiet"
                  disabled={starting}
                  title="Hold the run where it is. Its worktree stays as it is."
                  onclick={() => void startWork.pause(projectId, issueKey, flow.id)}
                  >Pause</Button
                >
              {/if}
              <Button
                size="sm"
                variant="quiet"
                disabled={starting}
                title="End the run. Its worktree, its branch and any move already sent stay as they are."
                onclick={() => void startWork.cancel(projectId, issueKey, flow.id)}
                >Stop run</Button
              >
            </span>
          {/if}
        </header>
        {#if job.state === 'ready' && open && planned}
          <p class="c-issue-run__note">
            Its worktree is ready, and {planned.planner || planned.planning
              ? `its planning reads “${planRunWords(planned)}”`
              : 'this project names no planner, so the run waits here'}{waitingQuestions > 0
              ? `, with ${waitingQuestions} question${waitingQuestions === 1 ? '' : 's'} for you`
              : ''}.
            <Button size="sm" variant="link" onclick={() => (work.tab = 'plan')}
              >Open the Plan tab</Button
            >
          </p>
        {/if}
        {#if job.detail && (job.state === 'interrupted' || job.state === 'setup_failed' || job.state === 'failed')}
          <p class="c-issue-run__note c-status--warn">{job.detail}</p>
        {/if}
        {#if open && (job.state === 'interrupted' || job.state === 'setup_failed')}
          <p class="c-issue-run__actions">
            <Button
              size="sm"
              variant="accent"
              disabled={starting || started?.working}
              title="Runs the project's setup in the worktree that is there, or makes the worktree again from the same plan when it is not"
              onclick={() => void startWork.resume(projectId, issueKey, flow.id, false)}
              >{job.state === 'setup_failed' ? 'Retry setup' : 'Resume'}</Button
            >
            {#if boundWorktree}
              <Button
                size="sm"
                disabled={starting || started?.working}
                onclick={() => void startWork.resume(projectId, issueKey, flow.id, true)}
                >Use as it is</Button
              >
            {/if}
          </p>
        {/if}
        <dl class="o-facts o-facts--wide">
          <dt>Worktree</dt>
          <dd>
            <span class="u-mono">{job.branch ?? job.worktree}</span>
            {#if boundWorktree}
              <Button
                size="sm"
                variant="link"
                onclick={() => onopenworktree(boundWorktree.id)}>Open</Button
              >
            {:else if job.state === 'creating' || job.state === 'interrupted' || job.state === 'failed'}
              <span class="c-issue__source">not made yet</span>
            {/if}
          </dd>
          {#each started?.moves ?? [] as move (move.approval)}
            <dt>{CAUSE_WORDS[move.request.cause]}</dt>
            <dd>
              {moveWords(move)}
              {#if move.operation.lastError && move.state === 'failed'}
                <span class="c-issue__source">{move.operation.lastError}</span>
              {/if}
              {#if move.state === 'needs_you' || move.state === 'failed'}
                <span class="c-issue-run__actions">
                  {#if move.operation.attention?.kind === 'status_moved'}
                    <Button
                      size="sm"
                      disabled={starting}
                      onclick={() =>
                        void startWork.settleMove(
                          projectId,
                          issueKey,
                          move.approval,
                          'anyway',
                        )}>Move to {move.request.payload.to} anyway</Button
                    >
                  {:else}
                    <Button
                      size="sm"
                      disabled={starting}
                      onclick={() =>
                        void startWork.settleMove(
                          projectId,
                          issueKey,
                          move.approval,
                          'retry',
                        )}>Retry</Button
                    >
                  {/if}
                  <Button
                    size="sm"
                    variant="quiet"
                    disabled={starting}
                    onclick={() =>
                      void startWork.settleMove(
                        projectId,
                        issueKey,
                        move.approval,
                        'leave',
                      )}>Leave it</Button
                  >
                </span>
              {/if}
            </dd>
          {/each}
        </dl>
        {#if (started?.moves.length ?? 0) > 0}
          <p class="c-issue-run__race">{raceNote(trackerName)}</p>
        {/if}
        {#if job.setupSession && (job.state === 'setting_up' || job.state === 'setup_failed')}
          <div class="c-issue-run__terminal">
            <Terminal session={job.setupSession} active={visible} />
          </div>
        {/if}
      </section>
    {/if}
    {#if trackerQueue.detailErrors[issueKey]}
      <p class="c-issue__error c-status--danger" role="alert">
        Could not read {issueKey} from {trackerName}: {trackerQueue.detailErrors[issueKey]}
      </p>
    {/if}
    <dl class="o-facts o-facts--wide">
      <dt>Team status</dt>
      <dd>
        {#if issue}
          {issue.status.name}{role
            ? ` · ${startsHere ? 'Start column' : ROLE_WORDS[role]}`
            : ' · not mapped'}
          {#if started && !open}
            <span class="c-issue__source"
              >{started.startMove.kind === 'move'
                ? `Start moves it to ${started.startMove.to}.`
                : STAY_WORDS[started.startMove.reason]}</span
            >
          {/if}
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
          None yet. Start work finds one that carries {issueKey}, or makes one.
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
