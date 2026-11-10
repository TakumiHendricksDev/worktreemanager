<script lang="ts">
  /**
   * What agents have said to each other lately, newest first: who to whom, how it went, and the
   * start of what was said.
   *
   * The wires in the tree show this while it is happening and then fade; this is where it stays, and
   * it is what a screen reader gets in place of the drawing. Clicking a row peeks at the session that
   * received the message. A polite live region announces exchanges that involve Home itself — the
   * conversation the user is having — and nothing else, since a fan-out of twenty children
   * announcing each answer would be a recital.
   */
  import { fleet } from '../state/fleet.svelte';
  import { sessions } from '../state/sessions.svelte';
  import { view } from '../state/view.svelte';
  import { isHome } from '../home';
  import type { AgentExchange } from '../ipc/types';

  const recent = $derived([...fleet.messages].reverse());

  const STATE_WORD: Record<AgentExchange['state'], string> = {
    in_flight: 'waiting',
    answered: 'answered',
    failed: 'failed',
  };

  const time = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });

  function involvesHome(exchange: AgentExchange): boolean {
    return [exchange.from, exchange.to].some((session) =>
      isHome(sessions.paneBySession(session)?.worktreeId),
    );
  }

  /** The latest exchange with Home, as a sentence, for the live region. */
  const spoken = $derived.by(() => {
    const last = [...fleet.messages].reverse().find(involvesHome);
    if (!last) return '';
    const from = fleet.nameOf(last.from);
    const to = fleet.nameOf(last.to);
    return last.state === 'in_flight'
      ? `${from} sent ${to} a message.`
      : `${to} ${STATE_WORD[last.state]} ${from}.`;
  });

  function peek(exchange: AgentExchange) {
    const pane = sessions.paneBySession(exchange.to);
    if (pane && !isHome(pane.worktreeId)) view.peek(pane.id);
  }
</script>

<section class="c-activity" aria-label="Activity">
  <p class="u-visually-hidden" aria-live="polite">{spoken}</p>
  {#if recent.length === 0}
    <p class="c-activity__empty">
      When one agent asks another for something — a handoff, a review, a message from Home —
      it shows here, and as a wire in the tree while it is happening.
    </p>
  {:else}
    <ul class="o-plain-list c-activity__list">
      {#each recent as exchange (exchange.id)}
        <li class="c-activity__item">
          <button class="c-activity__row" onclick={() => peek(exchange)}>
            <span class="c-activity__head">
              <span
                class="c-activity__dot c-activity__dot--{exchange.state}"
                aria-hidden="true"
              ></span>
              <span class="c-activity__who">
                {fleet.nameOf(exchange.from)} → {fleet.nameOf(exchange.to)}
              </span>
              <span class="c-activity__state">{STATE_WORD[exchange.state]}</span>
              <time
                class="c-activity__time"
                datetime={new Date(exchange.sentAt).toISOString()}
              >
                {time.format(exchange.sentAt)}
              </time>
            </span>
            <span class="c-activity__text">“{exchange.prompt}”</span>
            {#if exchange.reply}
              <span class="c-activity__text c-activity__text--reply"
                >↳ {exchange.reply}</span
              >
            {:else if exchange.error}
              <span class="c-activity__text c-activity__text--error"
                >↳ {exchange.error}</span
              >
            {/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</section>
