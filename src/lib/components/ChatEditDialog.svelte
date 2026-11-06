<script lang="ts">
  import { chatEdit } from '../state/chat-edit.svelte';
  import { sessions } from '../state/sessions.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Field from './ui/Field.svelte';
  import Button from './ui/Button.svelte';
  import Banner from './ui/Banner.svelte';
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
      <p>
        Continue in a new conversation from before this message. The original conversation
        and its later replies remain available. Files already changed by the agent are not
        reverted.
      </p>
      {#if chatEdit.busy}<p role="status">
          {chatEdit.attempted ? 'Creating conversation branch…' : 'Checking conversation…'}
        </p>{/if}
      {#if chatEdit.message}
        <Field id="chat-edit-message" label="Your message">
          <textarea
            id="chat-edit-message"
            class="c-input c-chat-edit__input"
            rows="8"
            bind:value={chatEdit.text}
            disabled={chatEdit.busy || chatEdit.outcome !== null}></textarea>
        </Field>
      {/if}
      {#if chatEdit.problem}<Banner variant="error">{chatEdit.problem}</Banner>{/if}
      {#if chatEdit.problem && chatEdit.snapshot?.status !== 'busy' && !chatEdit.outcome && !chatEdit.attempted && chatEdit.text}
        <p>
          You can copy this text to the composer as a new message instead. Your existing
          draft is kept, and nothing is sent automatically.
        </p>
      {/if}
      {#if chatEdit.outcome?.submitted}<Banner variant="info"
          >The edited message was sent in a new conversation. Your original history is
          retained.</Banner
        >{/if}
    {/snippet}
    {#snippet footer()}
      <Button variant="quiet" disabled={chatEdit.busy} onclick={original}
        >Open original</Button
      >
      <Button disabled={chatEdit.busy} onclick={() => chatEdit.close()}
        >{chatEdit.outcome ? 'Done' : 'Cancel'}</Button
      >
      {#if !chatEdit.outcome && chatEdit.snapshot?.status === 'busy'}
        <Button
          variant="accent"
          disabled={chatEdit.busy}
          onclick={() => chatEdit.stopOrRefresh()}
          >{chatEdit.stopRequested ? 'Check again' : 'Stop turn'}</Button
        >
      {:else if !chatEdit.outcome && !chatEdit.attempted && chatEdit.problem && chatEdit.text}
        <Button
          variant="accent"
          disabled={chatEdit.busy}
          onclick={() => chatEdit.copyToComposer()}>Copy to composer</Button
        >
      {:else if !chatEdit.outcome}<Button
          variant="accent"
          disabled={chatEdit.busy ||
            !!chatEdit.problem ||
            !chatEdit.text.trim() ||
            !chatEdit.message}
          onclick={() => chatEdit.submit()}>Create branch and send</Button
        >{/if}
    {/snippet}
  </Dialog>
{/if}
