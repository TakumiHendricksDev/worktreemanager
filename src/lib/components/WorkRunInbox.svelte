<script lang="ts">
  /**
   * The Work view's inbox column: what workflow runs need you to know or decide, kept until it is
   * resolved or you dismiss it.
   *
   * A question, an approval, a role that failed for good, a usage pause, a run a quit or a crash
   * cut off, a review that is ready. Each card goes to exactly what it is about — the question in
   * the focused Inbox, the role or the finding in its worktree's Reviews tab — and going there
   * settles nothing. Dismiss puts a card away without changing what it was about; the run's own
   * Reviews tab, or the focused Inbox, is where it is resumed, retried or answered.
   *
   * It lists every project's items, as Home's Needs you does, with the project's name on the
   * ones not on screen: the inbox is one list wherever it is drawn.
   */
  import { KIND_TONE, KIND_WORDS, filedAt, plain } from '../inbox';
  import type { InboxItem } from '../ipc/types';
  import { inbox } from '../state/inbox.svelte';
  import { work } from '../state/work.svelte';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';

  /** What a card's main button says. */
  function openWords(item: InboxItem): string {
    if (item.kind === 'question') return 'Answer…';
    if (item.kind === 'approval') return 'Review…';
    if (item.kind === 'review_ready') return 'Read the review';
    return 'Show';
  }
</script>

<div class="c-work__inbox-head">
  <h2 class="c-work__inbox-title" id="work-inbox-title">
    Inbox <span class="c-work__count">{inbox.items.length} open</span>
  </h2>
  {#if inbox.items.length > 0}
    <Button
      variant="quiet"
      size="sm"
      title="The inbox with its filters, and room to answer"
      onclick={() => (work.inboxOpen = true)}>Open</Button
    >
  {/if}
</div>
{#if inbox.items.length === 0}
  <p class="c-work__hint">
    Nothing needs you. Questions, approvals, failures, usage pauses and finished reviews
    land here, and stay until they are resolved or you dismiss them. Routine progress never
    does.
  </p>
{:else}
  <ul class="c-run-inbox">
    {#each inbox.items as item (item.id)}
      <li class="c-run-inbox__item">
        <span class="c-run-inbox__kind c-run-inbox__kind--{KIND_TONE[item.kind]}"
          >{KIND_WORDS[item.kind]}</span
        >
        <span class="c-run-inbox__title"><CodeText text={item.title} /></span>
        <span class="c-run-inbox__where" title={plain(item.body)}
          >{inbox.placeOf(item)} · {filedAt(item.createdAt)}</span
        >
        <span class="c-run-inbox__actions">
          <Button size="sm" onclick={() => inbox.open(item)}>{openWords(item)}</Button>
          <Button
            variant="quiet"
            size="sm"
            disabled={inbox.busy[item.id] ?? false}
            onclick={() => void inbox.dismiss(item)}>Dismiss</Button
          >
        </span>
      </li>
    {/each}
  </ul>
{/if}
