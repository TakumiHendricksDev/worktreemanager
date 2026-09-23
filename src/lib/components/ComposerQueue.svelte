<script lang="ts">
  /**
   * What a pane has waiting to be sent, just above the composer that wrote it.
   *
   * # Why it is in the foot and not the transcript
   *
   * A queued message has not been sent, and the transcript is the record of what was. Drawing one
   * there would show the agent a message it has not seen — and when it did go, the transcript would
   * either hold it twice or move it. It belongs with the composer, as something still being
   * written: the same place the context and skills panels sit, and for the same reason.
   *
   * # Why an entry is edited in place
   *
   * Rather than lifted back into the composer. The composer may already hold the next message, and
   * a queue is an order: an entry taken out and resubmitted joins the back of it, which silently
   * changes what the user asked for. Only the text is editable — attachments are removed by
   * removing the entry, which is rare enough not to earn a second set of controls.
   */
  import type { Attachment } from 'svelte/attachments';

  import { sessions, type Pane, type QueuedTurn } from '../state/sessions.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    pane,
    label,
    steersMidTurn,
  }: {
    pane: Pane;
    /** The agent, by name, for the sentences that say what it will do. */
    label: string;
    /** See `Capability.steersMidTurn`: whether "Send now" stops the turn to get in. */
    steersMidTurn: boolean;
  } = $props();

  /**
   * The text of each entry being edited, until it is saved.
   *
   * Local rather than on the entry, so an edit that is abandoned leaves the queued text exactly as
   * it was — and so a turn finishing mid-edit sends nothing half-typed. `editQueued` is what holds
   * the queue while one is open.
   */
  let drafts = $state<Record<string, string>>({});

  const head = $derived(pane.queue.find((entry) => !entry.steered) ?? null);

  /** What the queue will do next, as one phrase. */
  const next = $derived.by(() => {
    if (head === null) return `Sent · waiting for ${label} to read it`;
    if (pane.queueHeld) return 'Paused · send one to carry on';
    if (head.editing) return 'Waiting for your edit';
    if (pane.working) return `Sends when ${label} finishes`;
    return 'Sending…';
  });

  const steerTitle = $derived(
    !pane.working
      ? 'Send this now, ahead of the rest'
      : steersMidTurn
        ? `Send now — ${label} reads it at its next step, without stopping`
        : `Send now — stops ${label}'s current turn to send this`,
  );

  const focusOnce: Attachment<HTMLTextAreaElement> = (node) => {
    node.focus();
    node.setSelectionRange(node.value.length, node.value.length);
  };

  function edit(entry: QueuedTurn) {
    drafts = { ...drafts, [entry.id]: entry.text };
    sessions.editQueued(pane.id, entry.id, true);
  }

  function close(entry: QueuedTurn, save: boolean) {
    const text = drafts[entry.id] ?? entry.text;
    const rest = { ...drafts };
    delete rest[entry.id];
    drafts = rest;
    if (save) sessions.rewriteQueued(pane.id, entry.id, text);
    else sessions.editQueued(pane.id, entry.id, false);
  }

  /** The composer's own keys, so an edit is finished the way a message is sent. */
  function onKeydown(event: KeyboardEvent, entry: QueuedTurn) {
    if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') {
      event.preventDefault();
      close(entry, true);
    } else if (event.key === 'Escape') {
      event.preventDefault();
      close(entry, false);
    }
  }
</script>

<section class="c-queue" aria-label="Queued messages">
  <div class="c-queue__head">
    <strong>Queued · {pane.queue.length}</strong>
    <!-- Polite, because it changes when a turn ends — which is exactly when a screen-reader user
         wants to know the queue moved, and never worth interrupting them for. -->
    <span class={pane.queueHeld ? 'c-status--warn' : 'c-status--muted'} aria-live="polite"
      >{next}</span
    >
  </div>

  <ol class="o-plain-list c-queue__list">
    {#each pane.queue as entry (entry.id)}
      <li class="c-queue__item" class:is-steered={entry.steered}>
        {#if entry.editing}
          <!-- Not `bind:value`: an entry can be marked editing with no draft here — this list
               remounted mid-edit — and falling back to the queued text is what keeps that entry
               editable rather than stuck holding the queue behind an editor nobody can see. -->
          <textarea
            class="c-textarea c-queue__edit"
            aria-label="Edit queued message"
            rows="3"
            value={drafts[entry.id] ?? entry.text}
            oninput={(event) => (drafts[entry.id] = event.currentTarget.value)}
            onkeydown={(event) => onKeydown(event, entry)}
            {@attach focusOnce}></textarea>
          <div class="c-queue__actions">
            <Button variant="quiet" size="sm" onclick={() => close(entry, false)}
              >Cancel</Button
            >
            <Button
              variant="neutral"
              size="sm"
              title="Save (⌘↵)"
              onclick={() => close(entry, true)}>Save</Button
            >
          </div>
        {:else}
          <div class="c-queue__body">
            <p class="c-queue__text">{entry.text}</p>
            {#if entry.attachments.length > 0}
              <span class="c-queue__meta c-status--muted">
                <Icon name="file" size={11} />
                {entry.attachments.length === 1
                  ? entry.attachments[0]?.name
                  : `${entry.attachments.length} files`}
              </span>
            {/if}
            {#if entry.steered}
              <span class="c-queue__meta c-status--info">
                {steersMidTurn
                  ? `Sent · ${label} reads it at its next step`
                  : `Sent · stopping ${label}'s turn to deliver it`}
              </span>
            {/if}
          </div>
          <!-- Sent is sent: a steered entry has nothing left to change, so it has no controls and
               leaves the list when the provider echoes it. -->
          {#if !entry.steered}
            <div class="c-queue__actions">
              <Button
                variant="quiet"
                size="sm"
                title={steerTitle}
                onclick={() => void sessions.steerQueued(pane.id, entry.id)}
                >Send now</Button
              >
              <Button
                variant="quiet"
                size="sm"
                title="Change this message before it is sent"
                onclick={() => edit(entry)}>Edit</Button
              >
              <Button
                variant="quiet"
                size="sm"
                icon="sm"
                title="Remove — it will not be sent"
                ariaLabel="Remove queued message"
                onclick={() => sessions.removeQueued(pane.id, entry.id)}
              >
                <Icon name="close" size={11} />
              </Button>
            </div>
          {/if}
        {/if}
      </li>
    {/each}
  </ol>
</section>
