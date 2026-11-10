<script lang="ts">
  /**
   * A Claude or Codex session in the vendor's own terminal interface, with what WTM adds round it.
   *
   * The terminal is the provider's: its transcript, permission prompts and stop controls are drawn
   * by the CLI, and nothing here parses or scrapes them. What WTM owns is beside it — connection
   * and delivery notices, the Open Design review aside, and Connection info, which says plainly
   * what context and tools the session was given and what the adapter cannot see. A plain terminal
   * shows the same frame with every integration switched off.
   *
   * # The terminal is always mounted
   *
   * Before the session id exists too, for the reason `Terminal` gives: it buffers until it is told
   * its id, so the CLI's first output is never lost, and a pane that waited for the id also missed
   * the focus request a new pane is opened with. A restart keeps the same xterm for the same
   * reason. What the pane is doing while there is no process is written over the terminal instead.
   *
   * # Notices never resend
   *
   * An unknown delivery may have arrived, and a resumed conversation does not replay instructions,
   * so what a notice offers is to look (Check delivery, which says what it could and could not
   * read) or to answer for it: Mark not delivered for an unknown one, Withdraw for one still
   * waiting to be collected. Both only settle the record. A later Implement is a new, explicit send.
   */
  import { tick } from 'svelte';
  import { errorMessage } from '../ipc/types';
  import {
    NATIVE_STATUS,
    unresolved,
    type NativeReceipt,
    type NativeResolution,
  } from '../native-sessions';
  import type { PrototypeState } from '../open-design';
  import { extensionContext, extensionPart } from '../state/extensions.svelte';
  import { nativeSessions } from '../state/native-sessions.svelte';
  import { overlay } from '../state/overlay.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { settingsView } from '../state/settings-view.svelte';
  import ExtensionPoint from './ExtensionPoint.svelte';
  import PrototypeReview from './PrototypeReview.svelte';
  import Terminal from './Terminal.svelte';
  import Banner from './ui/Banner.svelte';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import Icon from './ui/Icon.svelte';

  let {
    pane,
    visible,
    label,
    terminal = $bindable(null),
    designOpen = $bindable(false),
    detailsOpen = $bindable(false),
    ondesignclose,
    onrestart,
  }: {
    pane: Pane;
    visible: boolean;
    /** The CLI's catalogue name, "Claude Code". */
    label: string;
    terminal?: ReturnType<typeof Terminal> | null;
    designOpen?: boolean;
    detailsOpen?: boolean;
    ondesignclose: () => void;
    /** The pane's own Reconnect, so a notice asks first exactly when the header's would. */
    onrestart: () => void;
  } = $props();
  let busy = $state(false);
  let error = $state<string | null>(null);
  /** What the last Check delivery said, shown until the next action here. */
  let checked = $state<string | null>(null);
  let review = $state<PrototypeState | null>(null);
  let reviewRegion = $state<HTMLElement | null>(null);
  const extension = extensionContext();
  const native = $derived(pane.session ? nativeSessions.states[pane.session] : undefined);
  const plain = $derived(pane.kind.kind === 'shell' && !!pane.kind.plain);
  const provider = $derived(pane.kind.kind === 'shell' ? (pane.kind.native ?? '') : '');
  const status = $derived(native?.status ?? (pane.error ? 'unavailable' : 'starting'));
  /** Nothing behind the pane: restored and not started yet, or refused for room when it was. */
  const stopped = $derived(pane.detached);
  /** A connection Reconnect is the answer to. A spawn failure has its own notice. */
  const recovery = $derived(
    !plain &&
      !pane.error &&
      !stopped &&
      (status === 'disconnected' || status === 'unavailable' || status === 'ended'),
  );
  /** Plain terminals have no receipts; an integrated pane lists earlier conversations' too. */
  const receipts = $derived(plain ? [] : (native?.deliveries.filter(unresolved) ?? []));
  /** Whether Claude-style collection applies: saved here, read by the CLI on the next message. */
  const collects = $derived(native?.capabilities.send === false);
  const canFallBack = $derived(
    !plain && (!!pane.error || status === 'unavailable' || status === 'disconnected'),
  );

  async function perform(action: () => Promise<unknown>) {
    busy = true;
    error = null;
    checked = null;
    try {
      await action();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  function checkDelivery() {
    const session = pane.session;
    if (!session) return;
    void perform(async () => {
      const result = await nativeSessions.check(session);
      checked =
        result.reason ??
        (result.changed
          ? 'The provider’s history confirms delivery.'
          : 'Still unconfirmed. Nothing was resent.');
    });
  }

  function resolve(receipt: NativeReceipt, resolution: NativeResolution) {
    const session = pane.session;
    if (!session) return;
    void perform(() => nativeSessions.resolve(session, receipt.id, resolution));
  }

  function receiveReview(next: PrototypeState | null) {
    review = next;
    const design = next?.prototype;
    // Only a revision still waiting on the user opens the panel by itself, and only once per pane:
    // the record of which one did outlives this component, so a Reconnect, a put-back or a restart
    // does not spring it open over the terminal again. A preview already looked at stays shut too,
    // which is what keeps a reload or a pop-out from reopening a review the user has seen.
    if (
      !design?.review ||
      design.implementedReview ||
      design.dismissed ||
      design.review.opened
    )
      return;
    const key = `${native?.conversation ?? pane.providerSession}:${design.id}:${design.review.id}`;
    if (nativeSessions.revealOnce(pane.id, key)) designOpen = true;
  }

  /** The notice's View review: the panel, with focus in it, so the keyboard is where the eye is. */
  async function viewReview() {
    designOpen = true;
    await tick();
    reviewRegion?.focus();
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

<div class="c-native" {@attach extensionPart('native.session', extension)}>
  <ExtensionPoint target="native.session" position="before" />
  {#if stopped && !pane.restoring}
    <Banner variant="info">
      Not running. {plain
        ? `Start opens a new plain ${label} terminal here.`
        : pane.providerSession
          ? `Start resumes this pane's ${label} conversation.`
          : `Start opens a new ${label} conversation here.`}
      {#snippet action()}
        <Button
          variant="inline"
          disabled={busy}
          onclick={() => void perform(() => sessions.startDetached(pane.id))}>Start</Button
        >
      {/snippet}
    </Banner>
  {:else if pane.error}
    <Banner variant="error">
      {plain
        ? `The plain ${label} terminal could not start: ${pane.error}`
        : `${label} could not start with WTM integration: ${pane.error}`}
      {#snippet action()}
        <span class="o-row c-native__actions">
          <Button variant="inline" disabled={busy} onclick={onrestart}
            >{plain ? 'Restart' : 'Reconnect'}</Button
          >
          {#if !plain}
            <Button variant="inline" onclick={() => (detailsOpen = true)}
              >Connection info</Button
            >
          {/if}
        </span>
      {/snippet}
    </Banner>
  {:else if recovery}
    <Banner variant="warn">
      {native?.error ??
        (status === 'disconnected'
          ? 'WTM lost its connection. The CLI may still be working; its state is unconfirmed.'
          : NATIVE_STATUS[status])}
      {#snippet action()}
        <span class="o-row c-native__actions">
          <Button
            variant="inline"
            disabled={busy}
            title="End the local CLI and resume this conversation; unconfirmed instructions are not resent"
            onclick={onrestart}>Reconnect</Button
          >
          {#if status === 'unavailable'}
            <Button variant="inline" onclick={() => (detailsOpen = true)}
              >Connection info</Button
            >
          {/if}
        </span>
      {/snippet}
    </Banner>
  {:else if !plain && native?.error}
    <Banner variant="warn">{native.error}</Banner>
  {/if}
  {#if error}
    <Banner variant="error">
      {error}
      {#snippet action()}
        <Button variant="inline" ariaLabel="Dismiss" onclick={() => (error = null)}>
          <Icon name="close" size={12} />
        </Button>
      {/snippet}
    </Banner>
  {/if}
  {#if checked}
    <Banner variant="info">
      {checked}
      {#snippet action()}
        <Button variant="inline" ariaLabel="Dismiss" onclick={() => (checked = null)}>
          <Icon name="close" size={12} />
        </Button>
      {/snippet}
    </Banner>
  {/if}
  {#each receipts as receipt (receipt.id)}
    <Banner variant={receipt.status === 'unknown' ? 'warn' : 'info'}>
      {#if receipt.earlier}From an earlier conversation in this pane:
      {/if}<strong>{receipt.label}.</strong>
      {receipt.status === 'unknown'
        ? 'WTM cannot confirm it arrived, and will not send it again.'
        : receipt.earlier
          ? 'Saved, not delivered. Only that conversation collects it.'
          : collects
            ? 'Saved, not delivered yet. Your next message in the terminal collects it.'
            : 'Saved, not sent yet.'}
      {#snippet action()}
        <span class="o-row c-native__actions">
          {#if receipt.status === 'unknown'}
            <Button
              variant="inline"
              disabled={busy}
              title="Ask what the provider's history shows. Nothing is resent."
              onclick={checkDelivery}>Check delivery</Button
            >
            <Button
              variant="inline"
              disabled={busy}
              title="You looked and it never arrived. Implement can then send it again on purpose; nothing is resent now."
              onclick={() => resolve(receipt, 'not_delivered')}>Mark not delivered</Button
            >
          {:else}
            <Button
              variant="inline"
              disabled={busy}
              title="Take it back before the CLI collects it. Nothing is sent."
              onclick={() => resolve(receipt, 'withdrawn')}>Withdraw</Button
            >
          {/if}
          {#if !receipt.earlier}
            <Button variant="inline" onclick={() => terminal?.focus()}
              >Focus terminal</Button
            >
          {/if}
        </span>
      {/snippet}
    </Banner>
  {/each}
  {#if !plain && !designOpen && review?.prototype.review && !review.prototype.implementedReview && receipts.length === 0}
    <Banner variant="info">
      {review.stale
        ? 'This design needs an update before implementation.'
        : 'Design ready for your review.'}
      {#snippet action()}
        <Button variant="inline" ariaControls="native-design-{pane.id}" onclick={viewReview}
          >View review</Button
        >
      {/snippet}
    </Banner>
  {/if}
  <div class="c-native__workspace">
    <div class="c-native__terminal">
      <Terminal
        bind:this={terminal}
        session={pane.session}
        active={visible && !pane.ended}
        onexit={() => {}}
      />
      {#if !pane.session && !pane.error && !stopped}
        <p class="c-native__overlay" aria-live="polite">Starting the installed CLI…</p>
      {/if}
    </div>
    {#if !plain}
      <aside
        class="c-pane__aside"
        id="native-design-{pane.id}"
        aria-label="Open Design"
        tabindex="-1"
        hidden={!designOpen}
        bind:this={reviewRegion}
      >
        <header class="c-pane__aside-head">
          <strong>Open Design</strong>
          <Button size="sm" variant="quiet" onclick={ondesignclose}>Hide</Button>
        </header>
        <div class="c-pane__aside-body">
          {#if !review}
            <div class="o-stack">
              <strong>No design to review yet</strong>
              <p class="c-note">
                Ask for a design in the terminal. Reviews created in this conversation
                appear here.
              </p>
              <div class="o-row">
                <Button size="sm" variant="quiet" onclick={() => terminal?.focus()}
                  >Focus terminal</Button
                >
              </div>
            </div>
          {/if}
          <PrototypeReview {pane} onreview={receiveReview} />
        </div>
      </aside>
    {/if}
  </div>
  <ExtensionPoint target="native.session" position="after" />
</div>

{#if detailsOpen}
  <Dialog title="Connection info" onclose={() => (detailsOpen = false)} wide>
    {#snippet body()}
      <div class="o-stack">
        <dl class="o-facts">
          <dt>Experience</dt>
          <dd>{plain ? 'Plain terminal' : 'Native CLI with WTM integration'}</dd>
          <dt>Provider</dt>
          <dd>{label}</dd>
          {#if native?.version}
            <dt>Version</dt>
            <dd>{native.version}</dd>
          {/if}
          <dt>Conversation</dt>
          <dd class="u-mono">
            {native?.conversation ||
              (plain ? 'Not tracked in a plain terminal' : 'Not yet registered')}
          </dd>
          <dt>Connection</dt>
          <dd>
            {plain
              ? 'Not connected to WTM tools'
              : stopped
                ? 'Not running'
                : NATIVE_STATUS[status]}
          </dd>
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
        <!-- Activity has no built-in content to sit before or after, so both land here. -->
        <ExtensionPoint target="native.activity" position="before" />
        <ExtensionPoint target="native.activity" position="after" />
        <h3>Capabilities</h3>
        <!-- What this pane really offers. It has no send or stop control of its own: a message is
             typed in the terminal, and a turn is stopped there with the CLI's own key. -->
        <dl class="o-facts">
          <dt>Status and activity</dt>
          <dd>
            {plain
              ? 'Unavailable'
              : provider === 'codex'
                ? 'Provider protocol events'
                : 'Lifecycle hooks'}
          </dd>
          <dt>Messages from WTM</dt>
          <dd>
            {plain
              ? 'None'
              : collects
                ? 'Saved, then collected with the next message you send in the terminal'
                : 'Open Design approvals and revisions are sent to this conversation'}
          </dd>
          <dt>Delivery receipts</dt>
          <dd>
            {plain
              ? 'None'
              : collects
                ? 'Confirmed when the CLI acknowledges collecting an instruction'
                : 'Confirmed by the provider’s reply'}
          </dd>
          <dt>Stopping a turn</dt>
          <dd>In the terminal, with the CLI’s own control</dd>
          <dt>Permission decisions</dt>
          <dd>Native CLI</dd>
          <dt>Transcript widgets</dt>
          <dd>Provider-owned</dd>
        </dl>
        {#if canFallBack}<p>
            A plain terminal starts a separate session without WTM tools. It does not
            replace this conversation.
          </p>{/if}
      </div>
    {/snippet}
    {#snippet footer()}
      <!-- In a pane window both are forwarded to the main window, which owns Settings and the
           tiling a new pane joins. -->
      <Button
        variant="quiet"
        onclick={() => {
          detailsOpen = false;
          settingsView.show('plugins', 'defaults');
        }}>New-session defaults…</Button
      >
      {#if canFallBack}<Button
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
