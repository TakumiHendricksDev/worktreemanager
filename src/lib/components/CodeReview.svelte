<script lang="ts">
  /**
   * The Review drawer: every comment in the worktree, and the one button that turns them into a
   * message for an agent.
   *
   * # Which agent
   *
   * The split button's main half drafts into the agent you last focused in this worktree, the
   * same target browser comments and the Database tab use (`sessions.draftDestination`); its
   * chevron lists every agent here, popped-out ones too, each named by what it was first asked so
   * two Claude sessions can be told apart. Nothing is sent — see `code-review.ts`.
   */
  import { reference } from '../code-review';
  import type { CodeComment } from '../ipc/types';
  import { choice, item, popUp, separator, under } from '../native-menu';
  import { code } from '../state/code.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';

  const {
    worktreeId,
    comments,
    outdated,
    onjump,
    onsend,
    oncopy,
  }: {
    worktreeId: string;
    comments: readonly CodeComment[];
    outdated: (comment: CodeComment) => boolean;
    onjump: (comment: CodeComment) => void;
    /** Draft these into that pane, labelled as the menu named it. */
    onsend: (paneId: string, label: string, list: readonly CodeComment[]) => void;
    oncopy: (list: readonly CodeComment[]) => void;
  } = $props();

  const open = $derived(comments.filter((c) => c.status === 'open'));
  /** What Send sends: the open comments not drafted yet, or every open one when all have been. */
  const pending = $derived.by(() => {
    const unsent = open.filter((c) => c.sentTo === null);
    return unsent.length > 0 ? unsent : open;
  });
  const again = $derived(pending.length > 0 && pending.every((c) => c.sentTo !== null));
  const destination = $derived(sessions.draftDestination(worktreeId));

  /** The agents a comment can go to: every live one in this worktree. */
  const agents = $derived(
    sessions
      .panesIn(worktreeId)
      .filter((pane) => pane.kind.kind === 'agent' && !pane.detached),
  );

  /** Two Claude sessions look the same by provider; what each was first asked tells them apart. */
  function agentLabel(pane: Pane): string {
    const name = pane.agentTitle ?? sessions.labelOf(pane);
    const first = pane.events.find((event) => event.kind === 'user_echo');
    const asked = first?.kind === 'user_echo' ? first.text.trim().split('\n')[0] : '';
    const topic = asked
      ? ` — “${asked.length > 40 ? `${asked.slice(0, 39)}…` : asked}”`
      : '';
    const out = sessions.isOut(pane.id) ? ' (own window)' : '';
    return `${name}${topic}${out}`;
  }

  const primaryLabel = $derived.by(() => {
    const pane = destination ? sessions.paneById(destination.id) : null;
    return pane ? (pane.agentTitle ?? sessions.labelOf(pane)) : null;
  });

  function send(pane: Pane): void {
    onsend(pane.id, pane.agentTitle ?? sessions.labelOf(pane), pending);
  }

  function sendPrimary(): void {
    const pane = destination ? sessions.paneById(destination.id) : null;
    if (pane) send(pane);
  }

  function chooseAgent(button: HTMLElement): void {
    void popUp(
      [
        ...agents.map((pane) =>
          choice(agentLabel(pane), pane.id === destination?.id, () => send(pane)),
        ),
      ],
      under(button),
    );
  }

  function clearMenu(button: HTMLElement): void {
    const resolved = comments.filter((c) => c.status === 'resolved').length;
    void popUp(
      [
        item(
          `Clear ${resolved} Resolved`,
          () => void code.clearComments(worktreeId, true),
          resolved > 0,
        ),
        separator,
        item('Clear All Comments', () => void code.clearComments(worktreeId, false)),
      ],
      under(button),
    );
  }

  let chevron = $state<HTMLElement | null>(null);
  let clearButton = $state<HTMLElement | null>(null);
</script>

<aside class="c-code-review" aria-label="Review comments">
  <header class="c-code-review__head">
    <span class="c-code-review__title">Review</span>
    <span class="c-code-review__count">{open.length} open</span>
    <span class="c-code-review__spacer"></span>
    <span bind:this={clearButton}>
      <Button
        variant="quiet"
        size="sm"
        icon="sm"
        title="Clear comments"
        ariaLabel="Clear comments"
        ariaHaspopup="menu"
        onclick={() => clearButton && clearMenu(clearButton)}
        ><Icon name="more" size={14} /></Button
      >
    </span>
  </header>

  <ul class="c-code-review__list">
    {#each comments as comment (comment.id)}
      <li>
        <button
          type="button"
          class="c-code-review__item"
          class:is-resolved={comment.status === 'resolved'}
          title={`Go to ${reference(comment.path, comment.start, comment.end)}`}
          onclick={() => onjump(comment)}
        >
          <span class="c-code-review__where">
            {reference(comment.path, comment.start, comment.end)}
          </span>
          <span class="c-code-review__text">{comment.text}</span>
          <span class="c-code-review__tags">
            {#if comment.status === 'resolved'}<span>Resolved</span>{/if}
            {#if comment.sentTo}<span>Sent to {comment.sentTo}</span>{/if}
            {#if outdated(comment)}<span class="c-code-review__warn">Outdated</span>{/if}
          </span>
          {#if comment.note}<span class="c-code-review__note">{comment.note}</span>{/if}
        </button>
      </li>
    {/each}
  </ul>

  <footer class="c-code-review__foot">
    {#if agents.length > 0 && primaryLabel}
      <div class="c-split-button">
        <button
          type="button"
          class="c-split-button__action"
          disabled={pending.length === 0}
          title="Put these comments in the agent's message box. Nothing is sent until you press Enter there."
          onclick={sendPrimary}
        >
          <span class="c-split-button__label"
            >{again ? 'Send again' : `Send ${pending.length}`} to {primaryLabel}</span
          >
        </button>
        <button
          type="button"
          class="c-split-button__menu"
          bind:this={chevron}
          aria-label="Send to another agent"
          aria-haspopup="menu"
          disabled={pending.length === 0}
          onclick={() => chevron && chooseAgent(chevron)}
          ><Icon name="chevron-down" size={12} /></button
        >
      </div>
    {:else}
      <p class="c-code-review__none">Open an agent in Sessions to send these to it.</p>
    {/if}
    <Button
      variant="quiet"
      size="sm"
      disabled={open.length === 0}
      title="Copy the open comments as Markdown"
      onclick={() => oncopy(open)}>Copy</Button
    >
  </footer>
</aside>
