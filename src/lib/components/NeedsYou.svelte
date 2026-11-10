<script lang="ts">
  /**
   * Every approval waiting on you, in every session in every project, oldest first — answerable
   * here, without going to the worktree it came from.
   *
   * # One open at a time
   *
   * The approval card is built to be read before it is answered: the whole command, the diff, the
   * plan. A stack of them open at once would put six of those on screen and the decision you are
   * making somewhere in the middle, so each is a row saying what it is and whose, and one is
   * expanded — the oldest, until you pick another. Answering opens the next.
   *
   * # Not Home's own
   *
   * Home's own approvals stay in Home's transcript, where the conversation that caused them is. This
   * is the list of everything *else*, which is the thing that had no single place to be answered.
   */
  import { shellRuns } from '../state/shell-runs.svelte';
  import { APPROVAL_WORD, summarize } from '../approvals';
  import { fleet, type Waiting } from '../state/fleet.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { view } from '../state/view.svelte';
  import { workspace } from '../state/workspace.svelte';
  import ApprovalCard from './ApprovalCard.svelte';
  import Button from './ui/Button.svelte';
  import Icon from './ui/Icon.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    onpeek,
    onreveal,
  }: {
    onpeek: (pane: Pane) => void;
    onreveal: (pane: Pane) => void;
  } = $props();

  /**
   * The approval the user opened, by id. Falls back to the oldest when it has been answered — but
   * not to one belonging to the pane being peeked at, whose card the peek is already showing. Two
   * copies of the same question on one screen read as two questions.
   */
  let chosen = $state<string | null>(null);
  const open = $derived(
    fleet.needsYou.find((w) => w.approval.id === chosen) ??
      fleet.needsYou.find((w) => w.pane.id !== view.peeked) ??
      null,
  );

  let listEl = $state<HTMLElement | null>(null);

  function where(pane: Pane): string {
    const project =
      workspace.projects.find((p) => p.id === pane.projectId)?.name ?? 'A project';
    const worktree =
      fleet.worktreesOf(pane.projectId)?.find((w) => w.id === pane.worktreeId)?.title ??
      pane.worktreeId.split('/').pop();
    return `${project} / ${worktree}`;
  }

  async function answer(waiting: Waiting, reply: Parameters<typeof sessions.answer>[2]) {
    // Whether the reader was working through the list, so the next card can take the caret too.
    const inside = listEl?.contains(document.activeElement) ?? false;
    chosen = null;
    await sessions.answerAndKeep(
      waiting.pane.id,
      waiting.approval.id,
      reply,
      waiting.approval.request,
    );
    if (inside) {
      queueMicrotask(() =>
        listEl
          ?.querySelector<HTMLElement>('.c-inbox__item.is-open .c-inbox__toggle')
          ?.focus(),
      );
    }
  }
</script>

{#if fleet.needsYou.length + shellRuns.pending.length > 0}
  <section class="c-inbox" aria-label="Needs you">
    <h2 class="c-inbox__heading">
      <Icon name="inbox" size={14} />
      Needs you
      <span class="c-inbox__count">{fleet.needsYou.length + shellRuns.pending.length}</span>
    </h2>
    <ul class="o-plain-list c-inbox__list" bind:this={listEl}>
      {#each shellRuns.pending as run (run.id)}
        <li class="c-inbox__item">
          <div class="c-inbox__row">
            <button class="c-inbox__toggle" onclick={() => shellRuns.reviewHome(run)}>
              <SessionDot status="attention" /><span class="c-inbox__what"
                >Home command</span
              >
              <span class="c-inbox__line" title={run.request.command}
                >{run.request.command}</span
              >
              <span class="c-inbox__who">{run.directory}</span>
            </button><Button size="sm" onclick={() => shellRuns.reviewHome(run)}
              >Review</Button
            >
          </div>
        </li>
      {/each}
      {#each fleet.needsYou as waiting (waiting.approval.id)}
        {@const summary = summarize(waiting.approval.request)}
        {@const expanded = open?.approval.id === waiting.approval.id}
        <li class="c-inbox__item" class:is-open={expanded}>
          <div class="c-inbox__row">
            <button
              class="c-inbox__toggle"
              aria-expanded={expanded}
              onclick={() => (chosen = waiting.approval.id)}
            >
              <SessionDot status="attention" />
              <span class="c-inbox__what">{APPROVAL_WORD[summary.kind]}</span>
              <span class="c-inbox__line" title={summary.line}>{summary.line}</span>
              <span class="c-inbox__who">
                {waiting.pane.agentTitle ?? sessions.labelOf(waiting.pane)} in {where(
                  waiting.pane,
                )}
              </span>
            </button>
            <span class="c-inbox__actions">
              <Button variant="quiet" size="sm" onclick={() => onpeek(waiting.pane)}
                >Peek</Button
              >
              <Button variant="quiet" size="sm" onclick={() => onreveal(waiting.pane)}
                >Open</Button
              >
            </span>
          </div>
          {#if expanded}
            <div class="c-inbox__card">
              <ApprovalCard
                request={waiting.approval.request}
                focusOnMount={false}
                onanswer={(reply) => void answer(waiting, reply)}
              />
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  </section>
{/if}
