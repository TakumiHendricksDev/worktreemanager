<script lang="ts">
  /**
   * One agent session: a header, its transcript, and a composer.
   *
   * # Why the composer stays enabled before the handshake finishes
   *
   * A session is spawned and its handshake is a network round trip, so there is a window — usually
   * brief, occasionally not — where the pane exists and the CLI is not ready. Disabling the input
   * for it would make a slow start look like a broken pane. The provider queues an early turn and
   * echoes it into the transcript immediately, so the message is visibly received either way.
   */
  import { agents, type AgentPane } from '../state/agents.svelte';
  import AgentTranscript from './AgentTranscript.svelte';
  import Button from './ui/Button.svelte';

  const { pane }: { pane: AgentPane } = $props();

  let draft = $state('');
  let scroller = $state<HTMLElement | null>(null);
  /**
   * Whether the view is following the tail.
   *
   * Tracked from a scroll listener rather than measured inside the anchoring effect, and the
   * difference is a real bug rather than a style choice. By the time that effect runs the DOM has
   * already grown, so measuring then answers "is it pinned *after* the append" — which on the
   * first content is `scrollTop === 0` against a tall scroller, reads as "scrolled away", and the
   * transcript never anchors at all. This records the answer from *before*.
   *
   * Not `$state`: only the effect reads it, and making it reactive would put the effect in its own
   * dependency set.
   */
  let pinned = true;

  const label = $derived(
    agents.options.find((o) => o.id === pane.provider)?.label ?? pane.provider,
  );

  /** True while a turn is in flight, so the control reads Stop rather than Send. */
  const running = $derived.by(() => {
    for (let i = pane.events.length - 1; i >= 0; i -= 1) {
      const kind = pane.events[i]?.kind;
      if (kind === 'turn_finished') return false;
      if (kind === 'turn_started') return true;
    }
    return false;
  });

  /*
   * Follow the tail as output arrives, unless the user has scrolled up.
   *
   * The guard is what makes reading a long transcript possible at all: an unconditional
   * scroll-to-bottom on every delta would yank the view back down mid-sentence.
   */
  $effect(() => {
    // Depend on the event count, not on `rows`: this must fire on append, and reading the folded
    // rows here would make the effect re-run on every text mutation inside a row as well.
    void pane.events.length;
    if (scroller && pinned) scroller.scrollTop = scroller.scrollHeight;
  });

  /**
   * Record whether the view is at the tail, so the effect above knows what to do next time.
   *
   * 32px of slack rather than an exact comparison, because sub-pixel layout means
   * `scrollTop + clientHeight` rarely equals `scrollHeight` exactly even when pinned.
   */
  function onScroll() {
    if (!scroller) return;
    pinned = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 32;
  }

  function submit(event: Event) {
    event.preventDefault();
    const text = draft.trim();
    if (!text || !pane.session) return;
    draft = '';
    void agents.send(pane.session, text);
  }

  /*
   * ⌘⏎ sends; a bare Enter inserts a newline.
   *
   * That way round because an agent prompt is routinely several lines — a stack trace, a diff, a
   * list of files — and a composer where Enter submits makes pasting one an accident. The chord is
   * checked here rather than on the form because a `<textarea>` never submits on Enter by itself.
   */
  function onKeydown(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') {
      submit(event);
    }
  }
</script>

<section class="c-agent" aria-label="{label} session">
  <header class="c-agent__head">
    <h2 class="c-agent__title">{label}</h2>

    {#if pane.error}
      <p class="c-agent__note c-status--danger">{pane.error}</p>
    {:else if pane.ended}
      <p class="c-agent__note c-status--warn">{pane.ended}</p>
    {:else if !pane.ready}
      <!-- Named rather than shown as a spinner: "starting" tells you what to expect, and the
           handshake is two round trips that can genuinely take a moment. -->
      <p class="c-agent__note c-status--subtle">starting…</p>
    {:else if running}
      <p class="c-agent__note c-status--info">working…</p>
    {/if}

    <div class="c-agent__actions">
      {#if pane.session && !pane.ended}
        <Button
          variant="quiet"
          size="sm"
          title="End this session and close the pane"
          onclick={() => void agents.close(pane.session ?? '')}
        >
          Close
        </Button>
      {/if}
    </div>
  </header>

  <div class="c-agent__body" bind:this={scroller} onscroll={onScroll}>
    {#if pane.events.length === 0 && pane.ready}
      <p class="c-agent__empty">
        This session works in the worktree's directory. Ask it something.
      </p>
    {/if}
    <AgentTranscript events={pane.events} />
  </div>

  <form class="c-agent__composer" onsubmit={submit}>
    <!-- svelte-ignore a11y_autofocus -->
    <textarea
      class="c-input c-agent__input"
      rows="3"
      placeholder="Ask {label}…  (⌘↵ to send)"
      aria-label="Message {label}"
      bind:value={draft}
      onkeydown={onKeydown}
      disabled={pane.ended !== null || pane.error !== null}></textarea>
    <div class="c-agent__send">
      {#if running && pane.session}
        <Button
          variant="neutral"
          size="sm"
          title="Stop the turn in progress"
          onclick={() => void agents.interrupt(pane.session ?? '')}
        >
          Stop
        </Button>
      {:else}
        <Button
          variant="accent"
          size="sm"
          type="submit"
          disabled={draft.trim().length === 0 || pane.ended !== null}
        >
          Send
        </Button>
      {/if}
    </div>
  </form>
</section>
