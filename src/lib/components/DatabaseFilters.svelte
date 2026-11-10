<script lang="ts">
  /**
   * WHERE and ORDER BY for table data, typed as SQL — PyCharm's data-editor bar.
   *
   * Free text rather than a builder, because the people using this write SQL, and a builder that
   * covered `ILIKE`, `IS DISTINCT FROM` and a subquery would be a worse way to type them. Rust runs
   * the page read-only with each fragment on lines of its own, so what is typed here can narrow and
   * order the rows and nothing more — see `table_page` in `wtm-db`.
   *
   * Enter applies and Esc goes back to what is applied. Each field holds a draft that follows the
   * applied value, so a sort toggle or "Filter by this value" elsewhere shows up here too.
   */
  const {
    filter,
    orderBy,
    onapply,
  }: {
    /** What the page on screen was fetched with. */
    filter: string;
    orderBy: string;
    onapply: (next: { filter: string; orderBy: string }) => void;
  } = $props();

  let whereDraft = $derived(filter);
  let orderDraft = $derived(orderBy);

  function key(event: KeyboardEvent): void {
    if (event.key === 'Enter') {
      event.preventDefault();
      onapply({ filter: whereDraft.trim(), orderBy: orderDraft.trim() });
    } else if (
      event.key === 'Escape' &&
      (whereDraft !== filter || orderDraft !== orderBy)
    ) {
      event.preventDefault();
      whereDraft = filter;
      orderDraft = orderBy;
    }
  }
</script>

<div class="c-database__filters">
  <label
    class="c-database__filter"
    class:is-applied={filter !== ''}
    class:is-dirty={whereDraft.trim() !== filter}
  >
    <span class="c-database__filter-label">WHERE</span>
    <input
      class="c-database__filter-input"
      value={whereDraft}
      oninput={(event) => (whereDraft = event.currentTarget.value)}
      onkeydown={key}
      placeholder="id > 10 AND name ILIKE '%plan%'"
      autocomplete="off"
      spellcheck="false"
    />
    <span class="c-database__filter-hint" aria-hidden="true">↵</span>
  </label>
  <label
    class="c-database__filter"
    class:is-applied={orderBy !== ''}
    class:is-dirty={orderDraft.trim() !== orderBy}
  >
    <span class="c-database__filter-label">ORDER BY</span>
    <input
      class="c-database__filter-input"
      value={orderDraft}
      oninput={(event) => (orderDraft = event.currentTarget.value)}
      onkeydown={key}
      placeholder="created_at DESC"
      autocomplete="off"
      spellcheck="false"
    />
    <span class="c-database__filter-hint" aria-hidden="true">↵</span>
  </label>
</div>
