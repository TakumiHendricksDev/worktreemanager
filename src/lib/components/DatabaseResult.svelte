<script lang="ts">
  /** A bounded result set returned by Rust. Values arrive as text so drivers stay out of the UI. */
  import type { QueryResult } from '../ipc/types';

  const {
    result,
    sortable = false,
    sortColumn = null,
    sortDirection = null,
    onsort,
  }: {
    result: QueryResult;
    sortable?: boolean;
    sortColumn?: string | null;
    sortDirection?: 'asc' | 'desc' | null;
    onsort?: (column: string) => void;
  } = $props();
</script>

{#if result.columns.length > 0}
  <div class="c-database__grid-wrap">
    <table class="c-database__grid">
      <thead>
        <tr>
          <th class="c-database__row-number" scope="col">#</th>
          {#each result.columns as column (column.name)}
            <th scope="col" title={column.typeName ?? undefined}>
              {#if sortable && onsort}
                <button
                  class="c-database__sort"
                  class:is-active={sortColumn === column.name}
                  onclick={() => onsort(column.name)}
                  title="Sort by {column.name}"
                >
                  {column.name}
                  {#if sortColumn === column.name}
                    <span
                      aria-label={sortDirection === 'desc' ? 'descending' : 'ascending'}
                    >
                      {sortDirection === 'desc' ? '↓' : '↑'}
                    </span>
                  {/if}
                </button>
              {:else}
                {column.name}
              {/if}
            </th>
          {/each}
        </tr>
      </thead>
      <tbody>
        {#each result.rows as row, rowIndex (rowIndex)}
          <tr>
            <th class="c-database__row-number" scope="row">{rowIndex + 1}</th>
            {#each row as cell, cellIndex (cellIndex)}
              <td title={cell.value ?? 'NULL'}>
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
</footer>
