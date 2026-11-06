<script lang="ts">
  import ExtensionPoint from './ExtensionPoint.svelte';
  import { extensionPart, extensionContext } from '../state/extensions.svelte';
  const publicContext = extensionContext();
  import { errorMessage } from '../ipc/types';
  import type { PrototypeState } from '../open-design';
  import { NATIVE_STATUS } from '../native-sessions';
  import { nativeSessions } from '../state/native-sessions.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import { overlay } from '../state/overlay.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Terminal from './Terminal.svelte';
  import PrototypeReview from './PrototypeReview.svelte';

  let {
    pane,
    visible,
    terminal = $bindable(null),
    designOpen = $bindable(false),
    detailsOpen = $bindable(false),
    ondesignclose,
  }: {
    pane: Pane;
    visible: boolean;
    terminal?: ReturnType<typeof Terminal> | null;
    designOpen?: boolean;
    detailsOpen?: boolean;
    ondesignclose: () => void;
  } = $props();
  let busy = $state(false);
  let error = $state<string | null>(null);
  let review = $state<PrototypeState | null>(null);
  let reviewRegion = $state<HTMLElement | null>(null);
  let revealedReview: string | null = null;
  const native = $derived(pane.session ? nativeSessions.states[pane.session] : undefined);
  const plain = $derived(pane.kind.kind === 'shell' && pane.kind.plain);
  const provider = $derived(pane.kind.kind === 'shell' ? (pane.kind.native ?? '') : '');
  const status = $derived(native?.status ?? (pane.error ? 'unavailable' : 'starting'));
  const recovery = $derived(
    !plain && ['disconnected', 'unavailable', 'ended'].includes(status),
  );
  const failure = $derived(error ?? pane.error ?? (!plain ? native?.error : null));
  const pending = $derived(
    plain
      ? []
      : (native?.deliveries.filter(
          (r) => r.status === 'pending' || r.status === 'unknown',
        ) ?? []),
  );

  async function perform(action: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await action();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function receiveReview(next: PrototypeState | null) {
    review = next;
    const key = next?.prototype.review
      ? `${native?.conversation}:${next.prototype.id}:${next.prototype.review.id}`
      : null;
    // Hiding a review survives status updates; only a different published revision reveals it.
    if (key && key !== revealedReview) designOpen = true;
    revealedReview = key;
  }

  function closeDesign(event: KeyboardEvent) {
    if (
      event.key !== 'Escape' ||
      event.defaultPrevented ||
      !visible ||
      !designOpen ||
      overlay.covering
    )
      return;
    const target = event.target;
    // Escape in xterm belongs to the vendor CLI, including its interrupt and permission controls.
    if (
      target instanceof Element &&
      (reviewRegion?.contains(target) ||
        target.getAttribute('aria-controls') === `native-design-${pane.id}`)
    ) {
      event.preventDefault();
      ondesignclose();
    }
  }
</script>

<svelte:window onkeydown={closeDesign} />

<ExtensionPoint target="native.session"
  ><div
    class="c-native"
    use:extensionPart={{ target: 'native.session', context: publicContext }}
  >
    {#if recovery}
      <div class="c-native__notice is-warning" role="status">
        <span
          >{failure ??
            (status === 'disconnected'
              ? 'WTM connection lost. Terminal state is unconfirmed.'
              : NATIVE_STATUS[status])}</span
        >
        <Button
          size="sm"
          variant="quiet"
          disabled={busy}
          title="Resume the saved conversation; uncertain instructions are not replayed"
          onclick={() => void perform(() => sessions.restart(pane.id))}>Reconnect</Button
        >
        {#if status === 'unavailable'}
          <Button size="sm" variant="quiet" onclick={() => (detailsOpen = true)}
            >Review connection</Button
          >
        {/if}
      </div>
    {:else if failure}
      <p class="c-native__notice is-warning c-status--danger" role="alert">{failure}</p>
    {/if}
    {#each pending as receipt (receipt.id)}
      <div
        class="c-native__notice"
        class:is-warning={receipt.status === 'unknown'}
        role="status"
      >
        <span
          ><strong>{receipt.label}.</strong>
          {receipt.status === 'pending'
            ? 'Saved. Send your next message in the native CLI to collect it.'
            : 'Delivery unknown. WTM will not resend this instruction.'}</span
        >
        <Button size="sm" variant="quiet" onclick={() => terminal?.focus()}
          >Focus terminal</Button
        >
        {#if receipt.status === 'unknown'}<Button
            size="sm"
            variant="quiet"
            disabled={busy}
            onclick={() => void perform(() => nativeSessions.load(pane.session!))}
            >Check delivery</Button
          >{/if}
      </div>
    {/each}
    {#if !plain && !designOpen && review?.prototype.review && pending.length === 0}
      <div class="c-native__notice" role="status">
        <span
          >{review.stale
            ? 'This design needs an update before implementation.'
            : 'Design ready for your review.'}</span
        >
        <Button size="sm" variant="quiet" onclick={() => (designOpen = true)}
          >View review</Button
        >
      </div>
    {/if}
    <div class="c-native__workspace">
      <div class="c-native__terminal">
        {#if pane.session}<Terminal
            bind:this={terminal}
            session={pane.session}
            active={visible && !pane.ended}
            onexit={() => {}}
          />
        {:else}<p class="c-pane__empty">
            {pane.error
              ? 'The native session could not start. Review its connection or open a separate plain terminal.'
              : 'Starting the installed CLI…'}
          </p>{/if}
      </div>
      {#if !plain}
        <aside
          class="c-native__aside"
          id="native-design-{pane.id}"
          aria-label="Open Design"
          hidden={!designOpen}
          bind:this={reviewRegion}
        >
          <div class="c-native__toolbar">
            <strong>Open Design</strong>
            <Button size="sm" variant="quiet" onclick={ondesignclose}>Hide</Button>
          </div>
          {#if !review}
            <section class="c-native__section">
              <h3>No design to review yet</h3>
              <p>
                Ask for a design in the terminal. Reviews created in this conversation
                appear here.
              </p>
              <Button size="sm" variant="quiet" onclick={() => terminal?.focus()}
                >Focus terminal</Button
              >
            </section>
          {/if}
          <PrototypeReview {pane} onreview={receiveReview} />
        </aside>
      {/if}
    </div>
  </div>
</ExtensionPoint>

{#if detailsOpen}
  <Dialog title="Connection info" onclose={() => (detailsOpen = false)} wide>
    {#snippet body()}
      <div class="o-stack">
        <dl class="o-facts">
          <dt>Experience</dt>
          <dd>{plain ? 'Plain terminal' : 'Native CLI with WTM integration'}</dd>
          <dt>Provider</dt>
          <dd>{native?.version ?? provider}</dd>
          <dt>Conversation</dt>
          <dd class="c-native__identity">{native?.conversation || 'Not yet registered'}</dd>
          <dt>Connection</dt>
          <dd>{plain ? 'Not connected to WTM tools' : NATIVE_STATUS[status]}</dd>
        </dl>
        <h3>Context supplied to this conversation</h3>
        <p>
          {plain
            ? 'WTM context is off.'
            : native?.contextSupplied
              ? 'Current workspace, WTM tool instructions, Open Design review rules and session-awareness guidance.'
              : 'Context is waiting for the native session to connect.'} Your global agent configuration
          is unchanged.
        </p>
        <h3>WTM tools</h3>
        <p>
          {plain
            ? 'Unavailable in a plain terminal.'
            : native?.tools.join(' · ') ||
              'Waiting for the adapter to report available tools.'}
        </p>
        <p class="c-note">
          {native?.toolsConnected
            ? 'The scoped tool connection has been observed.'
            : 'No tool connection has been observed yet.'} Delegated WTM agent panes require WTM
          Chat to be enabled.
        </p>
        <ExtensionPoint target="native.activity" />
        <h3>Capabilities</h3>
        <dl class="o-facts">
          <dt>Status and activity</dt>
          <dd>
            {plain
              ? 'Unavailable'
              : provider === 'codex'
                ? 'Provider protocol events'
                : 'Lifecycle hooks'}
          </dd>
          <dt>Send / stop from WTM</dt>
          <dd>
            {!plain && native?.capabilities.send
              ? 'Supported by this adapter'
              : 'Use native CLI input / Esc'}
          </dd>
          <dt>Design approval</dt>
          <dd>
            {plain
              ? 'Unavailable'
              : native?.capabilities.send
                ? 'Deliver to this conversation'
                : 'Collect on your next message; wait for acknowledgment'}
          </dd>
          <dt>Permission decisions</dt>
          <dd>Native CLI</dd>
          <dt>Transcript widgets</dt>
          <dd>Provider-owned</dd>
        </dl>
        {#if !plain && ['unavailable', 'disconnected'].includes(status)}<p>
            A plain terminal starts a separate session without WTM tools. It does not
            replace this conversation.
          </p>{/if}
      </div>
    {/snippet}
    {#snippet footer()}
      <Button
        variant="quiet"
        onclick={() => {
          detailsOpen = false;
          settingsView.show('plugins');
        }}>Session defaults…</Button
      >
      {#if !plain && ['unavailable', 'disconnected'].includes(status)}<Button
          onclick={() => {
            detailsOpen = false;
            void sessions.openAgent(pane.projectId, pane.worktreeId, provider, {
              beside: pane.id,
              experience: 'plain',
            });
          }}>Open plain terminal</Button
        >{/if}
      <Button onclick={() => (detailsOpen = false)}>Done</Button>
    {/snippet}
  </Dialog>
{/if}
