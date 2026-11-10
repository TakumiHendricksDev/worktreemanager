<script lang="ts">
  /**
   * Edit and continue: review an edited copy of a sent message before wtm forks the conversation.
   *
   * Mounted once per window and driven by the `chatEdit` store, because the request comes from a
   * plugin action (`ui.chat.edit`) that has no component of its own to open a dialog from. Nothing
   * is sent until Send in a new conversation: the edit forks the provider's history from the turn
   * before the message and leaves the original conversation intact. A busy turn is stopped through
   * the ordinary interrupt first, and an unsupported edit offers to copy the text into the composer
   * instead, again without sending it.
   *
   * "Conversation", not "branch": in a Git app a branch is a Git branch, and nothing here touches
   * one.
   *
   * After a failed send the one way forward is Retry, which asks again with the same request; see
   * `chat-edit.svelte.ts` for why that is safe and why the text is locked meanwhile.
   */
  import { chatEdit } from '../state/chat-edit.svelte';
  import { sessions } from '../state/sessions.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';

  const failed = $derived(chatEdit.attempted && !!chatEdit.problem && !chatEdit.outcome);
  const canCopy = $derived(
    !!chatEdit.problem &&
      chatEdit.snapshot?.status !== 'busy' &&
      !chatEdit.outcome &&
      !chatEdit.attempted &&
      !!chatEdit.text,
  );

  let input = $state<HTMLTextAreaElement | null>(null);
  let focused = false;
  // Dialog focuses its first control when it mounts, but this one mounts while the conversation is
  // still being checked: every control is disabled and the message is not there yet, so nothing
  // took focus. The message is focused once, as soon as it can be edited.
  $effect(() => {
    if (!chatEdit.opened) {
      focused = false;
      return;
    }
    if (input && !focused && !chatEdit.busy && !chatEdit.attempted) {
      focused = true;
      input.focus();
    }
  });

  function original() {
    const pane = sessions.paneById(chatEdit.originalPane);
    if (pane) sessions.focus(pane.worktreeId, pane.id);
    chatEdit.close();
  }
</script>

{#if chatEdit.opened}
  <Dialog
    title="Edit and continue"
    wide
    onclose={() => chatEdit.close()}
    closeDisabled={chatEdit.busy}
  >
    {#snippet body()}
      <div class="o-stack">
        <p>
          Continue in a new conversation from before this message. The original conversation
          and its later replies stay available. Files the agent already changed are not
          reverted.
        </p>
        {#if chatEdit.busy}<p role="status">
            {chatEdit.attempted ? 'Forking the conversation…' : 'Checking conversation…'}
          </p>{/if}
        {#if chatEdit.message}
          <Field
            id="chat-edit-message"
            label="Your message"
            help={chatEdit.attempted && !chatEdit.outcome
              ? 'Locked to this attempt, so it is never sent twice. To change it, close this and choose Edit again.'
              : null}
          >
            <textarea
              id="chat-edit-message"
              class="c-input c-chat-edit__input"
              rows="8"
              bind:this={input}
              bind:value={chatEdit.text}
              disabled={chatEdit.busy || chatEdit.attempted}></textarea>
          </Field>
        {/if}
        {#if chatEdit.problem}<Banner variant="error">{chatEdit.problem}</Banner>{/if}
        {#if failed}
          <p class="c-note">
            Retry asks wtm what became of this attempt and never sends the message twice. If
            a new conversation opened, it is already in this worktree, and Retry shows it.
          </p>
        {/if}
        {#if canCopy}
          <p>
            You can copy this text to the composer as a new message instead. Your existing
            draft is kept, and nothing is sent automatically.
          </p>
        {/if}
        {#if chatEdit.outcome?.submitted}<Banner variant="info"
            >The edited message was sent in a new conversation. Your original history is
            kept.</Banner
          >{/if}
      </div>
    {/snippet}
    {#snippet footer()}
      <Button variant="quiet" disabled={chatEdit.busy} onclick={original}
        >Open original</Button
      >
      <Button disabled={chatEdit.busy} onclick={() => chatEdit.close()}
        >{chatEdit.outcome ? 'Done' : 'Cancel'}</Button
      >
      {#if failed}
        <Button
          variant="accent"
          disabled={chatEdit.busy}
          onclick={() => void chatEdit.retry()}>Retry</Button
        >
      {:else if !chatEdit.outcome && chatEdit.snapshot?.status === 'busy'}
        <Button
          variant="accent"
          disabled={chatEdit.busy}
          onclick={() => chatEdit.stopOrRefresh()}
          >{chatEdit.stopRequested ? 'Check again' : 'Stop turn'}</Button
        >
      {:else if canCopy}
        <Button
          variant="accent"
          disabled={chatEdit.busy}
          onclick={() => chatEdit.copyToComposer()}>Copy to composer</Button
        >
      {:else if !chatEdit.outcome}<Button
          variant="accent"
          disabled={chatEdit.busy ||
            chatEdit.attempted ||
            !!chatEdit.problem ||
            !chatEdit.text.trim() ||
            !chatEdit.message}
          onclick={() => chatEdit.submit()}>Send in a new conversation</Button
        >{/if}
    {/snippet}
  </Dialog>
{/if}
