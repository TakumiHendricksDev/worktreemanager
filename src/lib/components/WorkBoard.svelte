<script lang="ts">
  /**
   * The team's board: the queue's issues in columns by team status, as the tracker has them.
   *
   * Display only. The columns are the project's mapped statuses (`[workflow.statuses]`) in the
   * workflow's role order, so they read left to right the way work moves. An issue whose status
   * nobody mapped sits in a last column that names those statuses: a gap in the mapping shows,
   * rather than hiding the issue. Nothing here moves an issue. Dragging a card, and the start
   * column's "Start work" drop, arrive with Start Work, the first item allowed to write to a
   * tracker; until then a card is a button that opens its issue.
   *
   * A card carries the run's words under the team status, the same two the queue row shows, so
   * the board never suggests that a column is what a run is doing.
   */
  import { work } from '../state/work.svelte';
  import {
    boardColumns,
    CATEGORY_TONE,
    currentSprint,
    ROLE_WORDS,
    runWords,
    SECTION_DOT,
    sprintEnds,
    syncedAt,
    TRACKER_NAMES,
    type TrackerQueue,
  } from '../tracker';

  const { queue }: { queue: TrackerQueue } = $props();

  const columns = $derived(boardColumns(queue));
  const sprint = $derived(currentSprint(queue));
  const trackerName = $derived(
    queue.tracker ? TRACKER_NAMES[queue.tracker] : 'the tracker',
  );
</script>

<section class="c-board" aria-label="Board">
  <header class="c-board__header">
    <div class="c-board__heading">
      <h1 class="c-board__title">{sprint ? sprint.name : 'Board'}</h1>
      <p class="c-board__meta">
        {#if sprint}
          <span>{sprint.goal ? `Goal: ${sprint.goal}` : 'No sprint goal set'}</span>
          {#if sprintEnds(sprint)}<span>{sprintEnds(sprint)}</span>{/if}
        {/if}
        <span>{syncedAt(queue.readAt)}</span>
      </p>
    </div>
    <p class="c-board__note">
      Columns are team statuses, read from {trackerName}. Moving an issue happens in
      {trackerName} for now.
    </p>
  </header>

  {#if queue.columns.length === 0}
    <p class="c-board__empty">
      No statuses are mapped, so every issue is in one column. Map the team's statuses in
      <code>[workflow.statuses]</code> in your own config to give the board its columns.
    </p>
  {/if}

  <div class="c-board__columns">
    {#each columns as column, index (column.status?.status ?? '')}
      <section class="c-board__column" aria-labelledby="board-column-{index}">
        <header class="c-board__column-head">
          <h2 class="c-board__column-title" id="board-column-{index}">
            {column.status ? column.status.status : 'Not mapped'}
            <span class="c-board__count">{column.issues.length}</span>
          </h2>
          <p class="c-board__role">
            {column.status ? ROLE_WORDS[column.status.role] : column.unmapped.join(', ')}
          </p>
        </header>
        {#if column.issues.length > 0}
          <ul class="o-plain-list c-board__cards">
            {#each column.issues as row (row.issue.key)}
              <li>
                <button
                  class="c-board-card"
                  class:is-selected={work.issue === row.issue.key}
                  title={row.issue.summary}
                  onclick={() => work.pickIssue(row.issue.key)}
                >
                  <span class="c-board-card__key">{row.issue.key}</span>
                  <span class="c-board-card__summary">{row.issue.summary}</span>
                  {#if column.status === null}
                    <span
                      class="c-team-status c-team-status--{CATEGORY_TONE[
                        row.issue.status.category
                      ]}">{row.issue.status.name}</span
                    >
                  {/if}
                  <span class="c-board-card__foot">
                    <span class="c-dot c-dot--{SECTION_DOT[row.section]}" aria-hidden="true"
                    ></span>
                    <span>{runWords(row.run)}</span>
                    {#if row.issue.assignee}
                      <span class="c-board-card__who">{row.issue.assignee}</span>
                    {/if}
                  </span>
                </button>
              </li>
            {/each}
          </ul>
        {:else}
          <p class="c-board__none">None</p>
        {/if}
      </section>
    {/each}
  </div>
</section>
