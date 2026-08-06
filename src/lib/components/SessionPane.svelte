<script lang="ts">
  /**
   * One session, whatever kind it is: a header, a body, and for an agent a composer.
   *
   * # Why one component for a shell and a chat
   *
   * Everything around the body is identical — the title, the status line, Restart and Close, the
   * focus handling, the "this session ended" state. Two components would be two copies of that, and
   * the copy would drift. The body is the only part that differs, so the body is the only part that
   * switches.
   *
   * The alternative — a `SessionPane` that only wraps and delegates — was tried and abandoned: it
   * meant every prop passed twice and every state class defined twice.
   */
  import { sessions, type Pane } from '../state/sessions.svelte';
  import AgentTranscript from './AgentTranscript.svelte';
  import ApprovalCard from './ApprovalCard.svelte';
  import ModelPicker from './ModelPicker.svelte';
  import Terminal from './Terminal.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    pane,
    visible,
  }: {
    pane: Pane;
    visible: boolean;
  } = $props();

  let draft = $state('');
  /**
   * True from submit until the turn is accepted or refused.
   *
   * A turn can now wait several seconds for a session that is still starting, so the control has to
   * say it is busy — and a second click during that wait would send the same text twice.
   */
  let sending = $state(false);
  let scroller = $state<HTMLElement | null>(null);
  let terminal = $state<ReturnType<typeof Terminal> | null>(null);
  /**
   * Whether the transcript is following its tail.
   *
   * Recorded from a scroll listener rather than measured in the anchoring effect: by the time that
   * effect runs the DOM has grown, so measuring then answers "is it pinned *after* the append" —
   * which on first content is `scrollTop === 0` against a tall scroller and reads as "scrolled
   * away". Not `$state`: only the effect reads it.
   */
  let pinned = true;

  /**
   * The provider id, or null for a shell.
   *
   * Narrowed once into a local rather than re-narrowed at each use: Svelte's template reads
   * `pane.kind` through a `$props()` getter, so TypeScript will not carry a narrowing across one.
   */
  const provider = $derived(pane.kind.kind === 'agent' ? pane.kind.provider : null);

  const label = $derived.by(() => {
    if (provider === null) return 'Shell';
    return sessions.options.find((o) => o.id === provider)?.label ?? provider;
  });

  const isFocused = $derived(sessions.focused[pane.worktreeId] === pane.id);

  /** True while a turn is in flight, so the control reads Stop rather than Send. */
  const running = $derived.by(() => {
    for (let i = pane.events.length - 1; i >= 0; i -= 1) {
      const kind = pane.events[i]?.kind;
      if (kind === 'turn_finished') return false;
      if (kind === 'turn_started') return true;
    }
    return false;
  });

  /** The oldest unanswered approval. One at a time, in arrival order. */
  const blocking = $derived(pane.approvals[0] ?? null);

  /**
   * Whether this provider's allow can carry a rewritten payload.
   *
   * Claude Code's `control_response` takes an `updatedInput`; Codex refuses the answer rather than
   * running the original unedited. A property of the protocol, not of the machine, which is why it
   * is keyed off the provider here — it belongs on the capability query.
   */
  const canEdit = $derived(provider === 'claude');

  const capability = $derived(
    provider === null ? null : (sessions.capabilities[provider] ?? null),
  );

  $effect(() => {
    void pane.events.length;
    if (scroller && pinned) scroller.scrollTop = scroller.scrollHeight;
  });

  /*
   * Move focus in when someone asks, and at no other time.
   *
   * Tracking `focusEpoch` alone is the design. An effect that also tracked the selection would fire
   * on every arrow key in the sidebar, and focus would land in a session the user was navigating
   * past. Same mechanism the terminal dock used, and the same reason.
   */
  $effect(() => {
    if (sessions.focusEpoch === 0) return;
    if (sessions.focusTarget !== pane.id) return;
    if (provider === null) terminal?.focus();
    else composer?.focus();
  });

  let composer = $state<HTMLTextAreaElement | null>(null);

  function onScroll() {
    if (!scroller) return;
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 32;
  }

  /*
   * Grow the composer to fit what is in it.
   *
   * `_pane.scss` claimed this for a while without anything implementing it — `rows="2"` fixed the
   * height and the `max-height: 33%` beside the claim resolved against a content-height form, so it
   * did nothing. The bounds live in CSS; this only sets the height between them, and past the
   * maximum the textarea's own `overflow-y` takes over.
   *
   * `height: auto` first, because `scrollHeight` never shrinks below the height already set — without
   * the collapse the box would grow with a long paste and never come back down when it was deleted.
   */
  $effect(() => {
    void draft;
    const el = composer;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${el.scrollHeight}px`;
  });

  async function submit(event: Event) {
    event.preventDefault();
    const text = draft.trim();
    if (!text || sending) return;

    /*
     * The draft is held until the turn is accepted, not cleared on the way out.
     *
     * Clearing first destroyed the message whenever `send` could not deliver it, which was every
     * turn composed before the session id landed — and nothing said so, because the composer looked
     * exactly like one that had just sent successfully.
     */
    sending = true;
    const sent = await sessions.send(pane.id, text);
    sending = false;
    // Only clear what actually went out. The wait can be seconds long on a pane that is still
    // starting, and anything typed during it is the next message rather than part of this one.
    if (sent && draft.trim() === text) draft = '';
  }

  /*
   * ⌘⏎ sends; a bare Enter inserts a newline.
   *
   * That way round because an agent prompt is routinely several lines — a stack trace, a diff, a list
   * of files — and a composer where Enter submits makes pasting one an accident.
   */
  function onKeydown(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') void submit(event);
  }
</script>

<!--
  Clicking anywhere in a pane makes it the split target for the next session, which is what an
  editor does. `noteFocus` rather than `focus`, so the click does not also re-trigger the focus
  effect and fight the caret the user just placed.
-->
<!--
  A click anywhere makes this the split target for the next session, which is what an editor does.
  There is no keyboard equivalent to add because `focusin` already covers it: tabbing into the pane
  is the keyboard way of doing the same thing, which is why the two handlers sit together.
-->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<section
  class="c-pane"
  class:is-focused={isFocused}
  aria-label="{label} session"
  onfocusin={() => sessions.noteFocus(pane.worktreeId, pane.id)}
  onclick={() => sessions.noteFocus(pane.worktreeId, pane.id)}
>
  <header class="c-pane__head">
    <h2 class="c-pane__title">{label}</h2>

    {#if pane.error}
      <p class="c-pane__note c-status--danger">{pane.error}</p>
    {:else if pane.ended}
      <p class="c-pane__note c-status--warn">{pane.ended}</p>
    {:else if provider !== null && !pane.ready}
      <p class="c-pane__note c-status--subtle">starting…</p>
    {:else if running}
      <p class="c-pane__note c-status--info">working…</p>
    {/if}

    <div class="c-pane__actions">
      {#if pane.ended || pane.error}
        <Button variant="neutral" size="sm" onclick={() => void sessions.restart(pane.id)}>
          Restart
        </Button>
      {:else if provider !== null}
        <Button
          variant="quiet"
          size="sm"
          icon="sm"
          title="Split this session to the right"
          ariaLabel="Split right"
          onclick={() =>
            void sessions.openAgent(
              pane.projectId,
              pane.worktreeId,
              provider ?? '',
              'right',
            )}
        >
          <Icon name="split-right" size={13} />
        </Button>
      {/if}
      <Button
        variant="quiet"
        size="sm"
        icon="sm"
        title="End this session and close the pane"
        ariaLabel="Close session"
        onclick={() => void sessions.close(pane.id)}
      >
        <Icon name="close" size={12} />
      </Button>
    </div>
  </header>

  {#if provider === null}
    <div class="c-pane__body c-pane__body--terminal">
      <Terminal
        bind:this={terminal}
        session={pane.session}
        active={visible && !pane.ended}
        onexit={() => {}}
      />
    </div>
  {:else}
    <div class="c-pane__body" bind:this={scroller} onscroll={onScroll}>
      {#if pane.events.length === 0 && pane.ready}
        <p class="c-pane__empty">Ask {label} something.</p>
      {/if}
      <AgentTranscript events={pane.events} />
    </div>

    {#if blocking}
      <!-- Above the composer and outside the scroller: the CLI does not continue the turn until this
           is answered, so a card that could be scrolled away would stall the session silently. -->
      <ApprovalCard
        request={blocking.request}
        {canEdit}
        onanswer={(answer) =>
          void sessions.answerAndKeep(pane.id, blocking.id, answer, blocking.request)}
      />
    {/if}

    <!--
      One card holding the message, what will run it, and the control that sends it.

      These were two strips: a settings row floating above a hairline, then the form. Nothing said
      the model belonged to the message you were writing, so the row read as pane chrome that had
      come loose. Both desktop clients put all three inside one bordered field for that reason, and
      it is also what lets the whole thing take the focus ring as a unit.
    -->
    <div class="c-pane__foot">
      <form class="c-composer" onsubmit={(event) => void submit(event)}>
        <!-- Deliberately NOT `.c-textarea`. That block is a bordered, filled form control, and its
             partial sorts after this one in `main.scss` — so at equal specificity it won, and the
             card ended up with a second bordered box and a resize grip inside it. The two
             declarations actually wanted from it are restated in `.c-composer__input`; see there. -->
        <textarea
          class="c-composer__input"
          bind:this={composer}
          placeholder="Ask {label}…"
          aria-label="Message {label}"
          bind:value={draft}
          onkeydown={onKeydown}
          disabled={pane.ended !== null || pane.error !== null}></textarea>

        <div class="c-composer__bar">
          <ModelPicker
            {capability}
            model={pane.model}
            effort={pane.effort}
            flags={pane.flags}
            disabled={pane.ended !== null || pane.error !== null}
            onchange={(next) => sessions.configure(pane.id, next)}
          />

          <div class="c-composer__send">
            {#if running}
              <Button
                variant="neutral"
                size="sm"
                onclick={() => void sessions.interrupt(pane.id)}
              >
                Stop
              </Button>
            {:else}
              <!-- The shortcut lives here rather than in the placeholder, where it was competing
                   with the prompt for the one line of text a user reads before typing. -->
              <span class="c-composer__hint" aria-hidden="true">⌘↵</span>
              <Button
                variant="accent"
                size="sm"
                type="submit"
                disabled={draft.trim().length === 0 || pane.ended !== null || sending}
              >
                {sending ? 'Sending…' : 'Send'}
              </Button>
            {/if}
          </div>
        </div>
      </form>
    </div>
  {/if}
</section>
