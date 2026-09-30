<script lang="ts">
  /**
   * A bounded result set returned by Rust. Values arrive as text so drivers stay out of the UI.
   *
   * # A grid of cells, not a page of text
   *
   * This used to be a plain `<table>` the browser selected text in. A drag ran in DOM order, so it
   * swept the rest of the starting row, every column between and the row numbers, and copying
   * brought all of that along. So the grid owns selection: text selection is off, a click takes
   * the whole cell, a drag or a shift-click spans a rectangle, and copy is written here — one cell
   * as its raw value, several as CSV.
   *
   * The handlers sit on the `<table>` and find their cell through `data-row`/`data-col`, rather
   * than on each of up to 500 × N cells. The table is also the focus target, with `role="grid"`,
   * so the arrow keys move the selection and screen readers are told what is selected.
   *
   * A right-click offers the other formats and hands the selection to an agent; Enter, Space or a
   * double-click opens the focused cell in `DatabaseValue`.
   */
  import { onDestroy, tick, untrack } from 'svelte';

  import {
    cellText,
    contains,
    rectOf,
    toCsv,
    toInsert,
    toJson,
    toTsv,
    type CellAt,
    type SelectedCells,
  } from '../database-grid';
  import type { DatabaseRelation, QueryCell, QueryResult } from '../ipc/types';
  import { item, popUp, separator, type MenuEntry } from '../native-menu';
  import DatabaseValue from './DatabaseValue.svelte';

  const {
    result,
    rowOffset = 0,
    sortable = false,
    sortColumn = null,
    sortDirection = null,
    onsort,
    relation = null,
    sendLabel = null,
    onsend,
  }: {
    result: QueryResult;
    /** Where this page starts in the whole relation, so page two is numbered from 101. */
    rowOffset?: number;
    sortable?: boolean;
    sortColumn?: string | null;
    sortDirection?: 'asc' | 'desc' | null;
    onsort?: (column: string) => void;
    /** The table these rows came from, when there is one: what an INSERT is written against. */
    relation?: DatabaseRelation | null;
    /** The agent a selection would be sent to, or null when this worktree has none. */
    sendLabel?: string | null;
    onsend?: (selection: SelectedCells) => void;
  } = $props();

  let grid = $state<HTMLTableElement | null>(null);
  let anchor = $state<CellAt | null>(null);
  let focus = $state<CellAt | null>(null);
  /** What a held button is extending, set on the press. Not state: nothing renders it. */
  let dragging: 'cells' | 'rows' | 'columns' | null = null;
  let copied = $state<string | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  /** Whether `DatabaseValue` is open. It shows whichever cell has focus. */
  let viewing = $state(false);

  /** Result row indices in display order. Every row today; find will narrow it. */
  const visible = $derived(result.rows.map((_, index) => index));
  const lastRow = $derived(visible.length - 1);
  const lastCol = $derived(result.columns.length - 1);
  const rect = $derived(anchor && focus ? rectOf(anchor, focus) : null);

  // A new result is a new grid. A selection addressed into the old one would name other data.
  $effect.pre(() => {
    void result;
    untrack(() => {
      anchor = null;
      focus = null;
      viewing = false;
    });
  });

  function select(from: CellAt, to: CellAt = from): void {
    anchor = from;
    focus = to;
  }

  function extendTo(to: CellAt): void {
    if (anchor) focus = to;
    else select(to);
  }

  /** Where a press landed. `col` is -1 on a row number, `row` is -1 on a column header. */
  function hit(target: EventTarget | null): CellAt | null {
    const element = (target as Element | null)?.closest?.('[data-row], [data-col]');
    if (!element) return null;
    return {
      row: Number(element.getAttribute('data-row') ?? -1),
      col: Number(element.getAttribute('data-col') ?? -1),
    };
  }

  function press(event: PointerEvent): void {
    if (event.button !== 0 || lastRow < 0) return;
    // The sort toggle is a real button; let it be pressed.
    if ((event.target as Element).closest('button')) return;
    const at = hit(event.target);
    if (!at) return;

    // Without this the compatibility mousedown starts a text selection — the old bug — and
    // `SessionTree` drags do the same for the same reason. It also cancels the focus a press
    // would give the table, so that is done by hand.
    event.preventDefault();
    grid?.focus({ preventScroll: true });

    const shift = event.shiftKey && anchor !== null;
    if (at.row === -1 && at.col === -1) {
      select({ row: 0, col: 0 }, { row: lastRow, col: lastCol });
    } else if (at.col === -1) {
      dragging = 'rows';
      if (shift && anchor)
        select({ row: anchor.row, col: 0 }, { row: at.row, col: lastCol });
      else select({ row: at.row, col: 0 }, { row: at.row, col: lastCol });
    } else if (at.row === -1) {
      dragging = 'columns';
      if (shift && anchor)
        select({ row: 0, col: anchor.col }, { row: lastRow, col: at.col });
      else select({ row: 0, col: at.col }, { row: lastRow, col: at.col });
    } else {
      dragging = 'cells';
      if (shift) extendTo(at);
      else select(at);
    }
  }

  function sweep(event: PointerEvent): void {
    if (!dragging) return;
    const at = hit(event.target);
    if (!at) return;
    if (dragging === 'rows' && at.row >= 0) focus = { row: at.row, col: lastCol };
    else if (dragging === 'columns' && at.col >= 0) focus = { row: lastRow, col: at.col };
    else if (dragging === 'cells' && at.row >= 0 && at.col >= 0) focus = at;
  }

  $effect(() => {
    const release = () => (dragging = null);
    window.addEventListener('pointerup', release);
    return () => window.removeEventListener('pointerup', release);
  });

  const MOVES: Record<string, [number, number]> = {
    ArrowUp: [-1, 0],
    ArrowDown: [1, 0],
    ArrowLeft: [0, -1],
    ArrowRight: [0, 1],
  };

  function step(from: CellAt, key: string, toEdge: boolean): CellAt | null {
    const move = MOVES[key];
    if (move) {
      const [down, across] = move;
      if (toEdge) {
        return {
          row: down < 0 ? 0 : down > 0 ? lastRow : from.row,
          col: across < 0 ? 0 : across > 0 ? lastCol : from.col,
        };
      }
      return {
        row: Math.min(lastRow, Math.max(0, from.row + down)),
        col: Math.min(lastCol, Math.max(0, from.col + across)),
      };
    }
    if (key === 'Home') return { row: toEdge ? 0 : from.row, col: 0 };
    if (key === 'End') return { row: toEdge ? lastRow : from.row, col: lastCol };
    return null;
  }

  async function keydown(event: KeyboardEvent): Promise<void> {
    if (lastRow < 0) return;
    const mod = event.metaKey || event.ctrlKey;
    if (mod && event.key === 'c') {
      event.preventDefault();
      await copy();
      return;
    }
    if (mod && event.key === 'a') {
      event.preventDefault();
      select({ row: 0, col: 0 }, { row: lastRow, col: lastCol });
      return;
    }
    if (event.key === 'Escape' && focus && rect && selectedCount() > 1) {
      event.preventDefault();
      select(focus);
      return;
    }
    if (event.key === 'Escape' && viewing) {
      event.preventDefault();
      viewing = false;
      return;
    }
    if ((event.key === 'Enter' || event.key === ' ') && !mod && focus) {
      event.preventDefault();
      viewing = !viewing;
      return;
    }
    const next = step(focus ?? { row: 0, col: 0 }, event.key, mod);
    if (!next) return;
    event.preventDefault();
    if (event.shiftKey) extendTo(next);
    else select(next);
    await tick();
    reveal(next);
  }

  function reveal(at: CellAt): void {
    grid
      ?.querySelector(`[data-row="${at.row}"][data-col="${at.col}"]`)
      ?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  }

  /** A keyboard user tabbing in lands on the first cell rather than on nothing. */
  function focused(): void {
    if (!focus && lastRow >= 0) select({ row: 0, col: 0 });
  }

  /** The selected cells as they arrived, in display order. */
  function selectedCells(): QueryCell[][] {
    if (!rect) return [];
    return visible
      .slice(rect.top, rect.bottom + 1)
      .map((index) => (result.rows[index] ?? []).slice(rect.left, rect.right + 1));
  }

  function selectedNames(): string[] {
    if (!rect) return [];
    return result.columns.slice(rect.left, rect.right + 1).map((column) => column.name);
  }

  /** The selected values as rows of text, in display order. */
  function selectedRows(): string[][] {
    if (!rect) return [];
    return visible
      .slice(rect.top, rect.bottom + 1)
      .map((index) =>
        (result.rows[index] ?? []).slice(rect.left, rect.right + 1).map(cellText),
      );
  }

  function selectedCount(): number {
    return rect ? (rect.bottom - rect.top + 1) * (rect.right - rect.left + 1) : 0;
  }

  /** One cell is its raw value; more is CSV. */
  function selectionText(): string | null {
    const rows = selectedRows();
    if (rows.length === 0) return null;
    const [only] = rows;
    return rows.length === 1 && only?.length === 1 ? (only[0] ?? '') : toCsv(rows);
  }

  function noteCopied(): void {
    const count = selectedCount();
    copied = `Copied ${count.toLocaleString()} cell${count === 1 ? '' : 's'}`;
    clearTimeout(copyTimer);
    copyTimer = setTimeout(() => (copied = null), 1400);
  }

  async function write(text: string | null): Promise<void> {
    if (text === null) return;
    try {
      await navigator.clipboard.writeText(text);
      noteCopied();
    } catch {
      /* Clipboard access can be denied. */
    }
  }

  function copy(): Promise<void> {
    return write(selectionText());
  }

  /**
   * Right-click. Pressed outside the selection it selects what was pressed first, as a spreadsheet
   * does, so the menu never acts on something other than what the pointer is on.
   */
  function menu(event: MouseEvent): void {
    const at = hit(event.target);
    if (!at || lastRow < 0) return;
    event.preventDefault();
    grid?.focus({ preventScroll: true });
    if (at.row >= 0 && at.col >= 0 && !contains(rect, at.row, at.col)) {
      select(at);
    } else if (at.col === -1 && at.row >= 0 && !contains(rect, at.row, 0)) {
      select({ row: at.row, col: 0 }, { row: at.row, col: lastCol });
    } else if (at.row === -1 && at.col >= 0 && !contains(rect, 0, at.col)) {
      select({ row: 0, col: at.col }, { row: lastRow, col: at.col });
    }

    const names = selectedNames();
    const single = selectedCount() === 1;
    const table = relation;
    const entries: MenuEntry[] = [
      item('Copy', () => void copy()),
      item('Copy with header', () => void write(toCsv([names, ...selectedRows()]))),
      {
        kind: 'submenu',
        text: 'Copy as',
        items: [
          item('CSV', () => void write(toCsv(selectedRows()))),
          item('TSV', () => void write(toTsv(selectedRows()))),
          item('JSON', () => void write(toJson(names, selectedCells()))),
          item(
            'SQL INSERT',
            () =>
              table &&
              void write(toInsert(table.schema, table.name, names, selectedCells())),
            table !== null,
          ),
        ],
      },
      item(
        names.length === 1 ? 'Copy column name' : 'Copy column names',
        () => void write(names.join(', ')),
      ),
      separator,
      item(single ? 'Open value' : 'Open focused value', () => (viewing = true)),
      separator,
      item(
        sendLabel ? `Send to ${sendLabel}` : 'Send to agent',
        () => onsend?.({ header: names, rows: selectedRows() }),
        sendLabel !== null && onsend !== undefined,
      ),
    ];
    void popUp(entries);
  }

  function open(event: MouseEvent): void {
    const at = hit(event.target);
    if (!at || at.row < 0 || at.col < 0) return;
    select(at);
    viewing = true;
  }

  /** The cell `DatabaseValue` shows: the focused one, looked up through the rows on screen. */
  const viewed = $derived.by(() => {
    if (!viewing || !focus) return null;
    const cell = result.rows[visible[focus.row] ?? -1]?.[focus.col];
    const column = result.columns[focus.col];
    return cell && column ? { cell, column } : null;
  });

  /*
   * Edit → Copy, as well as ⌘C.
   *
   * The menu's Copy asks WebKit to copy, which it only offers when there is a text selection —
   * and there never is one here. Cancelling `beforecopy` is WebKit's documented way for a page to
   * say it will handle the copy itself, and the `copy` event is where it does.
   */
  $effect(() => {
    const table = grid;
    if (!table) return;
    const before = (event: Event) => {
      if (rect) event.preventDefault();
    };
    const onCopy = (event: ClipboardEvent) => {
      const text = selectionText();
      if (text === null || !event.clipboardData) return;
      event.clipboardData.setData('text/plain', text);
      event.preventDefault();
      noteCopied();
    };
    table.addEventListener('beforecopy', before);
    table.addEventListener('copy', onCopy);
    return () => {
      table.removeEventListener('beforecopy', before);
      table.removeEventListener('copy', onCopy);
    };
  });

  onDestroy(() => clearTimeout(copyTimer));

  function sortGlyph(column: string): string {
    if (sortColumn !== column) return '⇅';
    return sortDirection === 'desc' ? '↓' : '↑';
  }
</script>

{#if result.columns.length > 0}
  <div class="c-database__result-body">
    <div class="c-database__grid-wrap">
      <table
        class="c-database__grid"
        role="grid"
        aria-multiselectable="true"
        aria-rowcount={visible.length}
        tabindex="0"
        bind:this={grid}
        onpointerdown={press}
        onpointerover={sweep}
        onkeydown={(event) => void keydown(event)}
        onfocus={focused}
        oncontextmenu={menu}
        ondblclick={open}
      >
        <thead>
          <tr>
            <th class="c-database__row-number" scope="col" data-row="-1" data-col="-1">#</th
            >
            <!-- Keyed by position: a join returns two columns called `id`, and keying by name
               made Svelte throw on the duplicate. -->
            {#each result.columns as column, col (col)}
              <th
                class="c-database__head"
                class:is-selected={rect !== null &&
                  rect.top === 0 &&
                  rect.bottom === lastRow &&
                  col >= rect.left &&
                  col <= rect.right}
                scope="col"
                data-col={col}
                title={column.typeName ?? undefined}
              >
                <span class="c-database__head-inner">
                  <span class="c-database__head-name">{column.name}</span>
                  {#if sortable && onsort}
                    <button
                      class="c-database__sort"
                      class:is-active={sortColumn === column.name}
                      onclick={() => onsort(column.name)}
                      title="Sort by {column.name}"
                      aria-label={sortColumn !== column.name
                        ? `Sort by ${column.name}`
                        : sortDirection === 'desc'
                          ? `Sorted by ${column.name}, descending`
                          : `Sorted by ${column.name}, ascending`}
                    >
                      {sortGlyph(column.name)}
                    </button>
                  {/if}
                </span>
              </th>
            {/each}
          </tr>
        </thead>
        <tbody>
          {#each visible as index, row (index)}
            <tr>
              <th
                class="c-database__row-number"
                class:is-selected={rect !== null &&
                  rect.left === 0 &&
                  rect.right === lastCol &&
                  row >= rect.top &&
                  row <= rect.bottom}
                scope="row"
                data-row={row}>{rowOffset + index + 1}</th
              >
              {#each result.rows[index] as cell, col (col)}
                {@const selected = contains(rect, row, col)}
                <td
                  class="c-database__cell"
                  class:is-selected={selected}
                  class:is-focus={focus !== null && focus.row === row && focus.col === col}
                  aria-selected={selected}
                  data-row={row}
                  data-col={col}
                  title={cell.value ?? 'NULL'}
                >
                  {#if cell.value === null}
                    <span class="c-database__null">NULL</span>
                  {:else}
                    <span class:has-more={cell.truncated}>{cell.value}</span>
                  {/if}
                </td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    {#if viewed}
      <DatabaseValue
        column={viewed.column.name}
        typeName={viewed.column.typeName}
        cell={viewed.cell}
        onclose={() => {
          viewing = false;
          grid?.focus({ preventScroll: true });
        }}
      />
    {/if}
  </div>
{:else if result.message}
  <div class="c-database__result-empty">{result.message}</div>
{:else}
  <div class="c-database__result-empty">
    Query complete. {result.affectedRows.toLocaleString()} row{result.affectedRows === 1
      ? ''
      : 's'}
    affected.
  </div>
{/if}

<footer class="c-database__result-meta">
  <span>{result.rows.length.toLocaleString()} row{result.rows.length === 1 ? '' : 's'}</span
  >
  <span>{result.durationMs.toLocaleString()} ms</span>
  {#if result.affectedRows > 0}<span>{result.affectedRows.toLocaleString()} affected</span
    >{/if}
  {#if result.truncated}<span class="c-status--warn">result truncated</span>{/if}
  {#if copied}<span class="c-status--ok" role="status">{copied}</span>{/if}
</footer>
