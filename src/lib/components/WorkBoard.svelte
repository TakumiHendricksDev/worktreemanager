<script lang="ts">
  /**
   * The team's board: the queue's issues in columns by team status, as the tracker has them.
   *
   * The columns are the project's mapped statuses (`[workflow.statuses]`) in the workflow's role
   * order, so they read left to right the way work moves. An issue whose status nobody mapped sits
   * in a last column that names those statuses: a gap in the mapping shows, rather than hiding the
   * issue. A card carries the run's words under the team status, the same two the queue row shows,
   * so the board never suggests that a column is what a run is doing.
   *
   * # Dragging a card
   *
   * - **The start column** — the first status mapped to `started` — is the only one that says
   *   "Start work", and dropping there is Start Work: the same action as the button, which starts
   *   the issue's run or focuses the one it has (`flow/start.rs`).
   * - **Any other column** is a change of team status and nothing else, through the outbox, which
   *   reads the issue first. Out of a `needs_review` status, it asks first. Backward, the run and its
   *   worktree are left exactly as they are, and the inbox offers to pause the run.
   * - An issue wtm has no run for moves only by Start: wtm writes to an issue for work it is doing.
   *
   * Pointer events rather than HTML5 drag-and-drop, for the reason `Sidebar` gives:
   * `dragDropEnabled` in `tauri.conf.json` turns `dragstart`/`drop` off across the webview. The
   * columns are measured when a press arms into a drag, and again as the board scrolls sideways
   * under a card held near its edge; the hit test is `columnAt`, which reads no DOM. A press that
   * never travels `ARM` pixels is a click, which opens the issue.
   */
  import { startWork } from '../state/start-work.svelte';
  import { work } from '../state/work.svelte';
  import { workspace } from '../state/workspace.svelte';
  import {
    boardColumns,
    CATEGORY_TONE,
    columnAt,
    currentSprint,
    ROLE_WORDS,
    runWords,
    sameStatus,
    SECTION_DOT,
    sprintEnds,
    startColumn,
    syncedAt,
    TRACKER_NAMES,
    type ColumnSlot,
    type IssueRow,
    type TrackerQueue,
  } from '../tracker';

  const { queue }: { queue: TrackerQueue } = $props();

  /** How far a press travels before it is a drag rather than a click. */
  const ARM = 5;
  /** How near the columns' left or right edge a dragged card scrolls them, and by how much. */
  const EDGE = 56;
  const SCROLL_STEP = 16;

  const columns = $derived(boardColumns(queue));
  const sprint = $derived(currentSprint(queue));
  const trackerName = $derived(
    queue.tracker ? TRACKER_NAMES[queue.tracker] : 'the tracker',
  );
  const start = $derived(startColumn(queue));

  let columnsEl = $state<HTMLElement | undefined>();
  /** The card being dragged and the column under it. */
  let drag = $state<{ key: string; from: string; over: string | null } | null>(null);
  /** A drag just ended on a card, whose click must not also open it. */
  let dragged = false;

  function measure(): ColumnSlot[] {
    if (!columnsEl) return [];
    return [...columnsEl.querySelectorAll<HTMLElement>('[data-board-status]')].map((el) => {
      const box = el.getBoundingClientRect();
      return { status: el.dataset.boardStatus ?? '', left: box.left, right: box.right };
    });
  }

  function press(event: PointerEvent, row: IssueRow): void {
    // Control-click is macOS's right-click.
    if (event.button !== 0 || event.ctrlKey) return;
    // Kills the compatibility mousedown, which would otherwise start a text selection that the
    // drag then drags across every column it passes. The click still arrives.
    event.preventDefault();
    const origin = { x: event.clientX, y: event.clientY };
    let armed = false;
    let slots: ColumnSlot[] = [];
    let lastX = origin.x;
    let frame = 0;

    const aim = () => {
      if (drag) drag = { ...drag, over: columnAt(slots, lastX) };
    };
    // A board wider than the window scrolls sideways while the pointer rests near an edge, and the
    // columns are measured again each frame: they move under a pointer that does not.
    const scroll = () => {
      frame = 0;
      const box = columnsEl?.getBoundingClientRect();
      if (!box || !columnsEl) return;
      const step =
        lastX < box.left + EDGE ? -SCROLL_STEP : lastX > box.right - EDGE ? SCROLL_STEP : 0;
      if (step === 0) return;
      columnsEl.scrollLeft += step;
      slots = measure();
      aim();
      frame = requestAnimationFrame(scroll);
    };

    const onMove = (m: PointerEvent) => {
      // Released over something that swallowed the `pointerup`: no drag is held any more.
      if ((m.buttons & 1) === 0) {
        finish(false);
        return;
      }
      lastX = m.clientX;
      if (!armed) {
        if (Math.hypot(m.clientX - origin.x, m.clientY - origin.y) < ARM) return;
        armed = true;
        slots = measure();
        drag = { key: row.issue.key, from: row.issue.status.name, over: null };
      }
      aim();
      if (frame === 0) frame = requestAnimationFrame(scroll);
    };
    const onUp = () => finish(true);
    const onCancel = () => finish(false);
    const onKey = (k: KeyboardEvent) => {
      if (k.key !== 'Escape') return;
      k.preventDefault();
      k.stopPropagation();
      finish(false);
    };
    const finish = (commit: boolean) => {
      if (frame !== 0) cancelAnimationFrame(frame);
      frame = 0;
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onCancel);
      window.removeEventListener('keydown', onKey, true);
      const target = commit && armed ? (drag?.over ?? null) : null;
      drag = null;
      if (armed) {
        dragged = true;
        setTimeout(() => (dragged = false), 0);
      }
      const projectId = workspace.activeProjectId;
      if (target && projectId && !sameStatus(target, row.issue.status.name))
        void startWork.drop(projectId, row.issue.key, target);
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onCancel);
    window.addEventListener('keydown', onKey, true);
  }

  /** What dropping the dragged card on `status` would do, for the column's hint. */
  function hintFor(status: string): string | null {
    if (!drag || sameStatus(status, drag.from)) return null;
    if (start && sameStatus(status, start)) return 'Start work';
    const at = queue.columns.findIndex((c) => sameStatus(c.status, status));
    const from = queue.columns.findIndex((c) => sameStatus(c.status, drag?.from ?? ''));
    return from >= 0 && at < from
      ? `Move back to ${status}: its run is left alone`
      : `Move to ${status}`;
  }
</script>

<section class="c-board" class:is-dragging={drag !== null} aria-label="Board">
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
      Columns are team statuses, read from {trackerName}.
      {#if start}
        Drop a card on {start} to start work; on any other column to change only its team status
        in
        {trackerName}.
      {/if}
    </p>
  </header>

  {#if startWork.boardError}
    <p class="c-board__error c-status--danger" role="alert">{startWork.boardError}</p>
  {/if}

  {#if queue.columns.length === 0}
    <p class="c-board__empty">
      No statuses are mapped, so every issue is in one column. Map the team's statuses in
      <code>[workflow.statuses]</code> in your own config to give the board its columns.
    </p>
  {/if}

  <div class="c-board__columns" bind:this={columnsEl}>
    {#each columns as column, index (column.status?.status ?? '')}
      {@const isStart =
        column.status !== null && start !== null && sameStatus(column.status.status, start)}
      {@const hint = column.status ? hintFor(column.status.status) : null}
      <section
        class="c-board__column"
        class:is-drop-target={hint !== null && drag?.over === column.status?.status}
        class:is-start-target={isStart && drag !== null && hint !== null}
        aria-labelledby="board-column-{index}"
        data-board-status={column.status?.status}
      >
        <header class="c-board__column-head">
          <h2 class="c-board__column-title" id="board-column-{index}">
            {column.status ? column.status.status : 'Not mapped'}
            <span class="c-board__count">{column.issues.length}</span>
          </h2>
          <p class="c-board__role">
            {#if isStart}
              <span class="c-board__start">Start work</span> · drop a card here to start it
            {:else}
              {column.status ? ROLE_WORDS[column.status.role] : column.unmapped.join(', ')}
            {/if}
          </p>
        </header>
        {#if hint && drag?.over === column.status?.status}
          <p class="c-board__hint" aria-live="polite">{hint}</p>
        {/if}
        {#if column.issues.length > 0}
          <ul class="o-plain-list c-board__cards">
            {#each column.issues as row (row.issue.key)}
              <li>
                <button
                  class="c-board-card"
                  class:is-selected={work.issue === row.issue.key}
                  class:is-dragged={drag?.key === row.issue.key}
                  class:is-busy={startWork.busy[row.issue.key] ?? false}
                  title={row.issue.summary}
                  onpointerdown={(event) => press(event, row)}
                  onclick={() => {
                    if (!dragged) work.pickIssue(row.issue.key);
                  }}
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
