<script lang="ts">
  /**
   * The Open Design review card for one conversation: preview, approve, revise, or cancel a design.
   *
   * The same card in a WTM Chat transcript and in a native CLI's review aside, because the review
   * is the same thing in both: an immutable revision bound to this conversation. How the approval
   * travels is what differs, and the card says so. In WTM Chat it is a turn, sent now. In a native
   * CLI it is a receipt: Codex is sent the instruction, and Claude Code collects it with the next
   * message the user types, so until then it is saved, not delivered.
   *
   * # What a native card shows after Implement
   *
   * The design, with its receipt's own status in place of the Implement button, until the user
   * dismisses it. Rust marks a native design implemented as soon as a receipt exists, and a card
   * that vanished then left "No design to review yet" standing over a design that had just gone
   * out. A rejected receipt clears that mark, so the card offers Implement again. What can be done
   * about an unresolved receipt — check it, mark it not delivered, withdraw it — is in the notice
   * above the terminal, which stays on screen when this panel is hidden.
   */
  import { untrack } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { commands } from '../ipc/commands';
  import { errorMessage } from '../ipc/types';
  import { unresolved, type NativeDeliveryStatus } from '../native-sessions';
  import type { PrototypeChanged, PrototypeState } from '../open-design';
  import { nativeSessions } from '../state/native-sessions.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import Button from './ui/Button.svelte';

  const {
    pane,
    onreview,
  }: {
    pane: Pane;
    onreview?: (review: PrototypeState | null) => void;
  } = $props();
  let records = $state<PrototypeState[]>([]);
  let feedback = $state('');
  let error = $state<string | null>(null);
  /** What cancelling left for the user to do, when WTM could not stop the turn itself. */
  let notice = $state<string | null>(null);
  let busy = $state(false);
  let selected = $state<string[]>([]);
  let contextChanged = $state(false);
  const isNative = $derived(pane.kind.kind === 'shell' && !!pane.kind.native);
  const native = $derived(pane.session ? nativeSessions.states[pane.session] : undefined);
  // Newest design still awaiting review. An approved or dismissed record stays saved,
  // but it must not hide Show preview and Implement for another design in this chat.
  const pending = $derived(
    records.filter((r) => !r.prototype.implementedReview && !r.prototype.dismissed),
  );
  /** A native design whose approval has a receipt, for when nothing else awaits review. */
  const sent = $derived(
    isNative
      ? (records.find(
          (r) => r.prototype.implementedReview && !r.prototype.dismissed && r.delivery,
        ) ?? null)
      : null,
  );
  const current = $derived(pending[0] ?? sent);
  const design = $derived(current?.prototype ?? null);
  const earlier = $derived(pending.slice(1));
  /**
   * Which conversation the review belongs to, for a native CLI only.
   *
   * A native `/clear` or `/resume` keeps the PTY but changes which review belongs here. A WTM Chat
   * pane's prototypes are its session's whatever its provider conversation, so a chat-edit fork,
   * which moves that conversation, must not throw away feedback typed into the card.
   */
  const conversation = $derived(
    isNative ? (native?.conversation ?? pane.providerSession) : null,
  );
  /** The review the card is about: the one approved, once there is one. */
  const reviewId = $derived(design?.implementedReview ?? design?.review?.id ?? null);
  /**
   * What became of this design's approval.
   *
   * The live receipt first, because a provider's acknowledgement moves it on without announcing the
   * design again; the design's own record of it otherwise, which covers a receipt this pane no
   * longer lists.
   */
  const delivery = $derived.by((): NativeDeliveryStatus | null => {
    if (!design || !reviewId) return null;
    const live = native?.deliveries.find(
      (r) => r.prototype === design.id && r.review === reviewId,
    );
    return live?.status ?? current?.delivery?.status ?? null;
  });
  /** Settled or on its way: Implement has nothing left to do for this revision. */
  const approved = $derived(
    !!design?.implementedReview && delivery !== null && delivery !== 'rejected',
  );

  /**
   * Why a native conversation cannot take an approval or a revision now, in words.
   *
   * Rust refuses both unless the CLI is idle and nothing in this conversation is still unresolved,
   * and a button greyed out for a reason nobody states reads as broken.
   */
  const nativeHold = $derived.by((): string | null => {
    if (!native) return null;
    const waiting = native.deliveries.find((r) => !r.earlier && unresolved(r));
    if (waiting)
      return `“${waiting.label}” is ${waiting.status === 'pending' ? 'still waiting to be collected' : 'of unknown delivery'}. Settle it in the notice above the terminal first.`;
    switch (native.status) {
      case 'idle':
        return null;
      case 'working':
        return 'Wait for the current turn in the terminal to finish.';
      case 'needs_approval':
        return 'Answer the permission prompt in the terminal first.';
      case 'starting':
        return 'Wait for the native session to connect.';
      default:
        return 'Reconnect the native session first.';
    }
  });

  /** The first reason Implement is unavailable, for the note under it. */
  const implementHold = $derived.by((): string | null => {
    if (!design?.review || approved || current?.stale) return null;
    if (nativeHold) return nativeHold;
    if (pane.working) return 'Wait for this chat’s turn to finish.';
    if (!design.review.opened)
      return 'Show the preview first: Implement approves what you saw.';
    if (feedback.trim().length > 0 || contextChanged)
      return 'Send your feedback as a revision, or clear it, first.';
    return null;
  });

  const DELIVERY: Record<NativeDeliveryStatus, string> = {
    delivered: 'Delivered to this conversation',
    pending: 'Approved · awaiting your next message',
    unknown: 'Delivery unknown',
    rejected: 'Not delivered',
  };

  // A native review stays subscribed while hidden, so a newly published revision can reveal it.
  $effect(() => {
    const review = current;
    untrack(() => onreview?.(review));
  });

  function accept(state: PrototypeState) {
    const next = [state, ...records.filter((r) => r.prototype.id !== state.prototype.id)];
    const shown = next.find(
      (r) => !r.prototype.implementedReview && !r.prototype.dismissed,
    );
    if (shown?.prototype.id !== design?.id) feedback = '';
    records = next;
    selected = shown?.prototype.sources.map((s) => s.path) ?? [];
    contextChanged = false;
  }

  $effect(() => {
    const session = pane.session;
    const owner = conversation;
    records = [];
    feedback = '';
    selected = [];
    contextChanged = false;
    error = null;
    notice = null;
    if (!session) return;
    let stopped = false;
    let updated = false;
    let off: (() => void) | undefined;
    void listen<PrototypeChanged>('prototype:changed', (event) => {
      if (!stopped && owner === conversation && event.payload.session === session) {
        updated = true;
        accept(event.payload.state);
      }
    }).then((unlisten) => {
      if (stopped) unlisten();
      else off = unlisten;
    });
    void commands
      .listPrototypes(session)
      .then((list) => {
        if (stopped || updated || owner !== conversation) return;
        records = list;
        selected =
          list
            .find((r) => !r.prototype.implementedReview && !r.prototype.dismissed)
            ?.prototype.sources.map((s) => s.path) ?? [];
      })
      .catch((e) => {
        if (!stopped) error = errorMessage(e);
      });
    return () => {
      stopped = true;
      off?.();
    };
  });

  /**
   * Run an action against this pane's session as it is now.
   *
   * Read at the click rather than asserted: a restart sets the session to null before the new one
   * exists, and an action then has nothing to address.
   */
  async function perform(action: (session: string) => Promise<unknown>) {
    const session = pane.session;
    if (!session) return;
    busy = true;
    error = null;
    notice = null;
    try {
      await action(session);
    } catch (e) {
      error = errorMessage(e);
    } finally {
      busy = false;
    }
  }

  async function cancel(session: string, id: string) {
    // A fixture or an older backend may answer nothing; the design is cancelled either way.
    const outcome = await commands.cancelPrototype(session, id);
    notice = outcome?.notice ?? null;
  }

  async function revise(refresh = false) {
    if (!design) return;
    const note =
      feedback.trim() ||
      (refresh
        ? 'Refresh the project context and regenerate the design.'
        : design.feedback.at(-1) || design.brief);
    const context = contextChanged
      ? ` Use these source paths in prototype_prepare: ${JSON.stringify(selected)}.`
      : '';
    const sent = await sessions.send(
      pane.id,
      `Revise Open Design prototype ${design.id}: ${note}. Call prototype_prepare with this id, then prototype_publish and wait for review.${context}`,
    );
    if (sent) {
      feedback = '';
      contextChanged = false;
    } else
      throw new Error(sessions.error ?? 'The feedback was not sent. Retry in this chat.');
  }
</script>

{#if design && current && !design.dismissed}
  <section class="c-design-first" aria-label="Prototype review">
    <div class="c-design-first__heading">
      <div class="c-design-first__identity">
        <span class="c-eyebrow">Open Design · this conversation</span>
        <h3 class="c-design-first__title">{design.title}</h3>
      </div>
      <div class="c-design-first__meta">
        <!-- The receipt's own status once there is one: "implementation approved" was written
             when the approval was sent, and says nothing about whether it arrived. -->
        <span class="c-note" role="status">
          {approved && delivery ? DELIVERY[delivery] : design.phase}
        </span>
        <Button
          size="sm"
          ariaLabel="Dismiss design review"
          title="Hide this review card; the saved design is kept"
          disabled={busy || !pane.session}
          onclick={() =>
            void perform(async (session) =>
              accept(await commands.dismissPrototype(session, design.id)),
            )}>Dismiss</Button
        >
      </div>
    </div>
    <p class="c-note">
      Uses {design.sources.filter((s) => s.kind === 'component').length} component references
      and {design.sources.filter((s) => s.kind === 'tokens').length} token/theme sources from
      this worktree, including local edits. {isNative
        ? 'The CLI in this pane creates the prototype.'
        : 'Your current chat agent creates the prototype.'}
    </p>
    {#if !approved}
      <details>
        <summary>Inspect project context</summary>
        <p class="c-note">
          Adjust the context for the next revision. Framework components are visually
          approximated in HTML; implementation uses the real APIs.
        </p>
        <div class="c-design-first__sources">
          {#each design.sources as source}
            <div class="c-design-first__source">
              <label class="c-design-first__choice">
                <input
                  type="checkbox"
                  value={source.path}
                  bind:group={selected}
                  onchange={() => (contextChanged = true)}
                />
                {source.path} · {source.kind}
              </label>
              <details>
                <summary>Source excerpt</summary>
                <pre class="c-design-first__excerpt">{source.text}</pre>
              </details>
            </div>
          {/each}
        </div>
      </details>
    {/if}
    {#if current.stale && !approved}
      <p class="c-status--danger">
        Project context changed. Refresh and review a new revision before implementation.
      </p>
    {/if}
    {#if design.review}
      <div class="c-design-first__actions">
        <Button
          size="sm"
          disabled={busy || !pane.session}
          onclick={() =>
            void perform((session) =>
              commands.openPrototypePreview(session, design.id, design.review!.id),
            )}>Show preview</Button
        >
        <Button
          variant="accent"
          size="sm"
          disabled={busy ||
            approved ||
            implementHold !== null ||
            current.stale ||
            !pane.session ||
            pane.ended !== null}
          onclick={() =>
            void perform((session) =>
              commands.implementPrototype(session, design.id, design.review!.id),
            )}
        >
          {approved && delivery ? DELIVERY[delivery] : 'Implement this design'}
        </Button>
      </div>
      {#if approved}<p class="c-note" role="status">
          {delivery === 'pending'
            ? 'Saved for this conversation, not delivered yet. Your next message in the terminal collects this exact approval.'
            : delivery === 'unknown'
              ? 'WTM cannot confirm delivery and will not send it again. Check the conversation; the notice above the terminal can check or settle it.'
              : 'The provider acknowledged this exact review. This is delivery confirmation, not implementation completion.'}
        </p>
      {:else}
        {#if implementHold}<p class="c-note" role="status">{implementHold}</p>{/if}
        <p class="c-note">
          {isNative
            ? native?.capabilities.send === false
              ? 'Implement saves this exact reviewed revision for this conversation. The CLI collects it with the next message you send in the terminal, under its current model and permissions.'
              : 'Implement sends this exact reviewed revision to this conversation with its current model and permissions.'
            : 'Implement sends this exact reviewed revision to this conversation with its current model and permissions.'}
        </p>
      {/if}
    {:else if design.phase === 'generating'}
      <p role="status">
        {pane.working
          ? 'Creating your prototype…'
          : 'No preview was published. Ask for a revision or retry below.'}
      </p>
      <div class="c-design-first__actions">
        {#if pane.working}
          <Button
            size="sm"
            disabled={busy || !pane.session}
            onclick={() => void perform((session) => cancel(session, design.id))}
            >Cancel design</Button
          >
        {:else}
          <Button
            size="sm"
            disabled={busy || !pane.session}
            onclick={() => void perform(() => revise())}>Retry design</Button
          >
        {/if}
      </div>
    {/if}
    {#if notice}<p class="c-note" role="status">{notice}</p>{/if}
    {#if !design.implementedReview}
      <label class="c-design-first__feedback">
        Feedback for this design
        <textarea
          class="c-input"
          rows="2"
          bind:value={feedback}
          placeholder="Try a simpler layout, keep the existing button style…"></textarea>
      </label>
      <div class="c-design-first__actions">
        <Button
          size="sm"
          disabled={busy ||
            pane.working ||
            nativeHold !== null ||
            (!feedback.trim() && !contextChanged && !current.stale) ||
            (design.sources.length > 0 && !selected.length) ||
            pane.ended !== null}
          onclick={() => void perform(() => revise(current.stale))}
          >{current.stale
            ? 'Refresh and revise'
            : isNative
              ? 'Revise in this conversation'
              : 'Revise in this chat'}</Button
        >
      </div>
      {#if nativeHold && !design.review}<p class="c-note" role="status">
          {nativeHold}
        </p>{/if}
    {/if}
    {#if earlier.length > 0}
      <details>
        <summary>Earlier prototypes in this conversation</summary>
        {#each earlier as record}
          <Button
            variant="link"
            size="sm"
            onclick={() => {
              const other = records.filter((r) => r !== record);
              records = [record, ...other];
              selected = record.prototype.sources.map((s) => s.path);
            }}>{record.prototype.title}</Button
          >
        {/each}
      </details>
    {/if}
    {#if error}<p role="alert" class="c-status--danger">{error}</p>{/if}
  </section>
{:else if error}
  <p class="c-note" role="alert">Prototype history: {error}</p>
{/if}
