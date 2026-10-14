<script lang="ts">
  import { sessions } from '../state/sessions.svelte';
  import type { Worktree } from '../ipc/types';
  import { errorMessage } from '../ipc/types';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  const {
    projectId,
    worktree,
    onclose,
  }: { projectId: string; worktree: Worktree; onclose: () => void } = $props();
  let brief = $state('');
  let provider = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  const existing = $derived(sessions.draftTargetIn(worktree.id));
  const available = $derived(sessions.options.filter((p) => p.available && p.offered));
  async function start() {
    busy = true;
    try {
      const paneId =
        existing?.id ??
        (await sessions.openAgent(
          projectId,
          worktree.id,
          provider || available[0]?.id || '',
        ));
      if (!paneId) throw new Error('Open a chat for this worktree to begin.');
      const sent = await sessions.send(
        paneId,
        `Prototype this UI using Open Design and our project components: ${brief}. Prepare the context, publish a rendered preview, and wait for my review before implementation.`,
      );
      if (!sent) throw new Error(sessions.error ?? 'The request was not sent.');
      onclose();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title="Prototype in chat" {onclose}>
  {#snippet body()}
    <div class="c-design-first">
      <p>
        Describe the UI. Your chat agent will use this worktree’s components and tokens,
        show a prototype, and wait for your implementation approval.
      </p>
      <p class="c-note">You can also ask directly in any worktree chat.</p>
      <label class="c-design-first__feedback"
        >Design brief<textarea
          class="c-input"
          rows="4"
          bind:value={brief}
          placeholder="Prototype the registration export using our components…"
        ></textarea></label
      >
      {#if existing}
        <p class="c-note">Continues in {sessions.labelOf(existing)}.</p>
      {:else}
        <label
          >Chat agent<select class="c-input" bind:value={provider}>
            {#each available as option}<option value={option.id}>{option.label}</option
              >{/each}
          </select></label
        >
      {/if}
      {#if error}<p role="alert" class="c-status--danger">{error}</p>{/if}
    </div>
  {/snippet}
  {#snippet footer()}
    <Button disabled={busy} onclick={onclose}>Cancel</Button>
    <Button
      variant="accent"
      disabled={busy || !brief.trim() || (!existing && !available.length)}
      onclick={() => void start()}>{busy ? 'Opening chat…' : 'Create prototype'}</Button
    >
  {/snippet}
</Dialog>
