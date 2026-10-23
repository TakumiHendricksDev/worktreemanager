<script lang="ts">
  /**
   * The focused Inbox: every open item on the left, filtered by what it asks of you, and the one
   * you picked open on the right.
   *
   * It takes the Work view's main column, beside the queue, when the title bar's Inbox is pressed
   * or a question or approval is clicked anywhere — those are answered here. Every project's items
   * are listed, with the project's name on those not on screen, because the inbox is one list:
   * Home's Needs you and the dock badge count the same items.
   *
   * # The item open
   *
   * The one picked, or the newest that the filter shows. Dismissing or answering it opens the next,
   * the way Home's Needs you opens the next approval.
   */
  import {
    FILTERS,
    KIND_ICON,
    KIND_TONE,
    KIND_WORDS,
    filedAt,
    inFilter,
    plain,
  } from '../inbox';
  import { codeRequests } from '../state/code-request.svelte';
  import { inbox } from '../state/inbox.svelte';
  import { work } from '../state/work.svelte';
  import CodeText from './CodeText.svelte';
  import InboxDetail from './InboxDetail.svelte';
  import InboxQuestion from './InboxQuestion.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import WorkSeatQueue from './WorkSeatQueue.svelte';

  const listed = $derived(inbox.items.filter((item) => inFilter(item, inbox.filter)));
  const open = $derived(
    listed.find((item) => item.id === inbox.chosen) ?? listed[0] ?? null,
  );
  const counts = $derived(
    Object.fromEntries(
      FILTERS.map((filter) => [
        filter.id,
        inbox.items.filter((item) => inFilter(item, filter.id)).length,
      ]),
    ) as Record<(typeof FILTERS)[number]['id'], number>,
  );

  function openFile(path: string, line: number | null): void {
    const item = open;
    if (!item?.project || !item.worktree) return;
    codeRequests.open(item.project, item.worktree, path, line);
  }
</script>

<div class="c-work-inbox">
  <section class="c-work-inbox__list" aria-labelledby="work-inbox-focused-title">
    <div class="c-work-inbox__head">
      <h1 class="c-work-inbox__title" id="work-inbox-focused-title">Inbox</h1>
      <span class="c-work-inbox__count"
        >{inbox.items.length} open · kept until resolved</span
      >
      <Button
        variant="quiet"
        size="sm"
        title="Back to the workspace"
        onclick={() => (work.inboxOpen = false)}>Close</Button
      >
    </div>
    <div class="c-inbox-filters" role="group" aria-label="Filter">
      {#each FILTERS as filter (filter.id)}
        <button
          class="c-inbox-filters__chip"
          class:is-selected={inbox.filter === filter.id}
          aria-pressed={inbox.filter === filter.id}
          onclick={() => (inbox.filter = filter.id)}
        >
          {filter.label}
          <span class="c-inbox-filters__count">{counts[filter.id]}</span>
        </button>
      {/each}
    </div>
    <WorkSeatQueue />
    {#if listed.length === 0}
      <p class="c-work__hint">
        {inbox.items.length === 0 ? 'Nothing needs you.' : 'Nothing under this filter.'}
      </p>
    {:else}
      <ul class="o-plain-list c-inbox-list">
        {#each listed as item (item.id)}
          <li>
            <button
              class="c-inbox-list__row"
              class:is-selected={open?.id === item.id}
              aria-current={open?.id === item.id ? 'true' : undefined}
              title={plain(item.title)}
              onclick={() => (inbox.chosen = item.id)}
            >
              <span class="c-inbox-list__icon c-inbox-list__icon--{KIND_TONE[item.kind]}">
                <Icon name={KIND_ICON[item.kind]} size={14} />
              </span>
              <span class="c-inbox-list__text">
                <span class="c-inbox-list__title"><CodeText text={item.title} /></span>
                <span class="c-inbox-list__meta"
                  >{KIND_WORDS[item.kind]} · {inbox.placeOf(item)} · {filedAt(
                    item.createdAt,
                  )}</span
                >
              </span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
    <p class="c-work-inbox__foot">
      Routine progress never lands here. Dismissing a macOS notification leaves its item
      open.
    </p>
  </section>

  <div class="c-work-inbox__detail">
    {#if open}
      {#if open.kind === 'question' && open.question}
        <InboxQuestion item={open} question={open.question} onopenfile={openFile} />
      {:else}
        <InboxDetail item={open} />
      {/if}
    {:else}
      <div class="c-placeholder">
        <p>Questions, approvals, failures and usage pauses from review runs land here.</p>
      </div>
    {/if}
  </div>
</div>
