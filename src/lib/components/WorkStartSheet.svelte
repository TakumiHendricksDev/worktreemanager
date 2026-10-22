<script lang="ts">
  /**
   * The Start sheet: where an issue's work should happen when there is more than one place, or
   * whether it may leave review.
   *
   * # A choice, in the plan's order
   *
   * Start opens this only when resolution found more than one candidate (`flow::start::resolve`):
   * the worktree a key is carried by, the project's offered branches, then a new worktree. The
   * first is picked to begin with, because the order is the plan's preference, and nothing is
   * recorded until Start is pressed here. What starting does to the team status is said under the
   * list, with the race a tracker leaves open, so the move is never a surprise.
   *
   * # Leaving review
   *
   * Moving an issue out of a `needs_review` status asks first, whether a Start or a drop asked for
   * it: someone may be reviewing it.
   *
   * Mounted once, by the queue, which is up whenever Work is: the button, ⌘↵ and the board all
   * open the same sheet through `startWork`.
   */
  import { TRACKER_NAMES } from '../tracker';
  import { startWork } from '../state/start-work.svelte';
  import { trackerQueue } from '../state/tracker.svelte';
  import {
    candidateId,
    candidateWords,
    candidatesOf,
    raceNote,
    STAY_WORDS,
  } from '../start-work';
  import Button from './ui/Button.svelte';
  import Choice from './ui/Choice.svelte';
  import Dialog from './ui/Dialog.svelte';

  const sheet = $derived(startWork.sheet);
  const trackerName = $derived(TRACKER_NAMES[trackerQueue.queue?.tracker ?? 'jira']);
  const candidates = $derived(
    sheet?.kind === 'choose' ? candidatesOf(sheet.preview.resolved) : [],
  );

  let picked = $state<string | null>(null);
  // A new sheet starts on its first candidate: the plan's preferred place.
  $effect(() => {
    picked = candidates[0] ? candidateId(candidates[0]) : null;
  });

  const key = $derived(
    sheet?.kind === 'choose'
      ? sheet.preview.key
      : sheet?.kind === 'confirm'
        ? sheet.key
        : '',
  );
  const busy = $derived(key ? (startWork.busy[key] ?? false) : false);

  function close(): void {
    startWork.sheet = null;
  }

  function submit(event: Event): void {
    event.preventDefault();
    if (sheet?.kind === 'choose' && picked) void startWork.choose(picked);
    else if (sheet?.kind === 'confirm') void startWork.confirm();
  }
</script>

{#if sheet?.kind === 'choose'}
  {@const move = sheet.preview.startMove}
  <Dialog
    title="Where should {sheet.preview.key}'s work happen?"
    onclose={close}
    onsubmit={submit}
    closeDisabled={busy}
    wide
  >
    {#snippet body()}
      <p class="c-start-sheet__lead">
        More than one place could hold it. Pick one; nothing is made or moved until you
        press Start.
      </p>
      <ul class="o-plain-list c-start-sheet__choices">
        {#each candidates as candidate (candidateId(candidate))}
          {@const words = candidateWords(candidate)}
          {@const id = candidateId(candidate)}
          <li>
            <Choice
              type="radio"
              name="start-candidate"
              checked={picked === id}
              onchange={(on) => {
                if (on) picked = id;
              }}
            >
              <span class="c-start-sheet__choice">
                <span class="u-mono">{words.title}</span>
                <span class="c-start-sheet__why">{words.why}</span>
              </span>
            </Choice>
          </li>
        {/each}
      </ul>
      {#if sheet.preview.problems.length > 0}
        <p class="c-start-sheet__note">
          No new worktree is offered: {sheet.preview.problems.join('; ')}
        </p>
      {/if}
      <p class="c-start-sheet__move">
        {#if move.kind === 'move'}
          Start also moves {sheet.preview.key} from {move.from} to {move.to} in {trackerName}.
        {:else}
          {STAY_WORDS[move.reason]}
        {/if}
      </p>
      {#if move.kind === 'move'}
        <p class="c-start-sheet__note">{raceNote(trackerName)}</p>
      {/if}
    {/snippet}
    {#snippet footer()}
      <Button variant="neutral" onclick={close} disabled={busy}>Cancel</Button>
      <Button variant="accent" type="submit" disabled={busy || picked === null}
        >{busy ? 'Starting…' : 'Start'}</Button
      >
    {/snippet}
  </Dialog>
{:else if sheet?.kind === 'confirm'}
  <Dialog
    title="Move {sheet.key} out of review?"
    onclose={close}
    onsubmit={submit}
    closeDisabled={busy}
  >
    {#snippet body()}
      <p>
        {sheet.key} is in {sheet.from}, so someone may be reviewing it. {sheet.then ===
        'start'
          ? `Starting work moves it to ${sheet.to} in ${trackerName}.`
          : `This moves it to ${sheet.to} in ${trackerName}.`}
        Its run and its worktree stay as they are.
      </p>
      <p class="c-start-sheet__note">{raceNote(trackerName)}</p>
    {/snippet}
    {#snippet footer()}
      <Button variant="neutral" onclick={close} disabled={busy}>Cancel</Button>
      <Button variant="accent" type="submit" disabled={busy}
        >{sheet.then === 'start'
          ? `Start and move to ${sheet.to}`
          : `Move to ${sheet.to}`}</Button
      >
    {/snippet}
  </Dialog>
{/if}
