<script lang="ts">
  /**
   * "Close session?" — the one confirmation every route to closing a pane goes through.
   *
   * The pane's own close button and Home's session tree both open this, so a session closed from
   * the tree is asked about, and closed, exactly as one closed from its header: `sessions.close`,
   * which takes the delegated children and the side question with it.
   *
   * It names what the close would end that is still going on, the way Restart's confirmation does:
   * a turn that is running stops, and an approval waiting on the user goes unanswered. A pane with
   * nothing behind it says that instead, because closing it ends nothing. A native CLI is named as
   * one, not as a shell: what closes is a Claude or Codex conversation's process, and the
   * instructions WTM could not confirm lose the only pane that lists them.
   */
  import { unresolved } from '../native-sessions';
  import { nativeSessions } from '../state/native-sessions.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';

  const {
    pane,
    onclose,
    onclosed,
  }: {
    pane: Pane;
    /** The dialog is done, whichever way. */
    onclose: () => void;
    /** The session was closed, before `onclose`. For a caller whose focus was on it. */
    onclosed?: () => void;
  } = $props();

  let closing = $state(false);

  const kind = $derived(pane.kind.kind);
  const asks = $derived(pane.approvals.length);
  const cli = $derived(
    pane.kind.kind === 'shell' && pane.kind.native
      ? `${sessions.providerLabel(pane.kind.native)} ${pane.kind.plain ? 'terminal' : 'CLI'}`
      : null,
  );
  const native = $derived(
    cli !== null && pane.session ? nativeSessions.states[pane.session] : undefined,
  );
  const unconfirmed = $derived(native?.deliveries.filter(unresolved).length ?? 0);

  async function confirmed() {
    if (closing) return;
    closing = true;
    try {
      await sessions.close(pane.id);
      onclosed?.();
      onclose();
    } finally {
      closing = false;
    }
  }
</script>

<Dialog title="Close session?" {onclose} closeDisabled={closing}>
  {#snippet body()}
    <p>
      {#if pane.detached}
        Nothing is running in this pane, so closing only removes it.
      {:else}
        Closing ends this {cli ??
          (kind === 'shell' ? 'shell' : kind === 'browser' ? 'browser' : 'conversation')} and
        removes the pane.
      {/if}
      {#if kind === 'agent'}
        Delegated children and any side question are closed with it.
      {/if}
    </p>
    {#if asks > 0}
      <p class="c-status--warn">
        It is waiting on you: {asks === 1
          ? 'the request will go unanswered.'
          : `${asks} requests will go unanswered.`}
      </p>
    {:else if native?.status === 'needs_approval'}
      <p class="c-status--warn">
        The permission prompt in its terminal goes unanswered, and the turn stops.
      </p>
    {:else if pane.working}
      <p class="c-status--warn">The current turn will be stopped.</p>
    {/if}
    {#if unconfirmed > 0}
      <p class="c-note">
        {unconfirmed === 1
          ? 'One WTM instruction is still unconfirmed. Its record is kept, but no pane will list it.'
          : `${unconfirmed} WTM instructions are still unconfirmed. Their records are kept, but no pane will list them.`}
      </p>
    {/if}
  {/snippet}
  {#snippet footer()}
    <Button variant="neutral" onclick={onclose} disabled={closing}>Cancel</Button>
    <Button variant="danger-solid" onclick={() => void confirmed()} disabled={closing}
      >{closing ? 'Closing…' : 'Close'}</Button
    >
  {/snippet}
</Dialog>
