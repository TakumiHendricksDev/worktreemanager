<script lang="ts">
  /**
   * The Work view's inbox: what review runs need you to know or decide, kept until you resolve it.
   *
   * The minimal set R5 files — a role that failed for good, a usage pause, a provider's permission
   * prompt in a run session, a run a quit or a crash cut off. Questions and approvals join it with
   * the items that first ask them. Resolving an item here is a person saying they have seen it; it
   * does not resume or retry anything, which the run's own Reviews tab does.
   */
  import { untrack } from 'svelte';

  import type { InboxKind } from '../ipc/types';
  import { reviewRuns } from '../state/review-runs.svelte';
  import CodeText from './CodeText.svelte';
  import Button from './ui/Button.svelte';

  const { projectId }: { projectId: string | null } = $props();

  const KIND_WORDS: Record<InboxKind, string> = {
    failure: 'Failure',
    capacity: 'Usage',
    permission: 'Permission',
    interrupted: 'Interrupted',
  };

  $effect(() => {
    const project = projectId;
    untrack(() => void reviewRuns.loadInbox(project));
  });
</script>

<h2 class="c-work__inbox-title" id="work-inbox-title">
  Inbox <span class="c-work__count">{reviewRuns.inbox.length}</span>
</h2>
{#if reviewRuns.inbox.length === 0}
  <p class="c-work__hint">
    Nothing needs you. Failures, usage pauses and interruptions from review runs land here,
    and stay until you resolve them.
  </p>
{:else}
  <ul class="c-run-inbox">
    {#each reviewRuns.inbox as item (item.id)}
      <li class="c-run-inbox__item">
        <span class="c-run-inbox__kind c-run-inbox__kind--{item.kind}"
          >{KIND_WORDS[item.kind]}</span
        >
        <span class="c-run-inbox__title"><CodeText text={item.title} /></span>
        <p class="c-run-inbox__body"><CodeText text={item.body} /></p>
        <Button size="sm" variant="quiet" onclick={() => void reviewRuns.resolve(item)}
          >Resolve</Button
        >
      </li>
    {/each}
  </ul>
{/if}
