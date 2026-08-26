<script lang="ts">
  /**
   * Every delegated session in this worktree, with room to act on each one.
   *
   * # Why a dialog and not a wider rail
   *
   * The rail sits above the panes, so every row it grows is a row the sessions lose — and the list
   * it has to hold is up to twenty long, because that is `spawn_agents`' own `MAX_TASKS`. Twenty
   * rows is not a band, it is a view.
   *
   * `Inspector.svelte` reached the same conclusion for the same worktree and its reasoning carries:
   * a persistent list would be a third region competing for `min-height: 0` and would need a
   * `z-index` this app does not spend, where a `Dialog` unmounts on close and costs nothing when
   * shut. The rail keeps the part that must be glanceable — is anything running, does anything need
   * me — and this is where the twenty live.
   *
   * # Why the rows carry verbs the rail does not
   *
   * Because until now a child that was never shown could not be reached at all: the rail offered
   * *show* and *split*, and closing lived only in a pane header, which a child without a tile does
   * not have. Show, split and close are the three things you can do to a delegated session, so all
   * three are here.
   */
  import { panesOf } from '../state/layout.svelte';
  import { AT_TILE_CAP, MAX_PANES_PER_WORKTREE, sessions } from '../state/sessions.svelte';
  import { STATUS_WORD } from '../status';
  import Button from './ui/Button.svelte';
  import Dialog from './ui/Dialog.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const {
    worktreeId,
    onclose,
  }: {
    worktreeId: string;
    onclose: () => void;
  } = $props();

  const visible = $derived(new Set(panesOf(sessions.layoutFor(worktreeId))));
  const groups = $derived(sessions.runsIn(worktreeId));
  const atTileCap = $derived(visible.size >= MAX_PANES_PER_WORKTREE);

  let closingId = $state<string | null>(null);
  let closingBusy = $state(false);
  const closing = $derived(
    groups.flatMap((g) => g.children).find((c) => c.id === closingId) ?? null,
  );

  function closeChild(id: string) {
    closingId = id;
  }

  async function closeConfirmed() {
    if (closingBusy) return;
    const id = closingId;
    if (!id) return;
    closingBusy = true;
    try {
      await sessions.close(id);
      closingId = null;
    } finally {
      closingBusy = false;
    }
  }
  function show(paneId: string, split: boolean) {
    sessions.showRelated(paneId, split);
    onclose();
  }
</script>

<Dialog title="Delegated agents" {onclose} wide>
  {#snippet body()}
    {#if groups.length === 0}
      <p>No agent has been delegated to in this worktree.</p>
    {:else}
      {#each groups as group (group.run)}
        <section class="c-agents__run">
          <h3 class="c-section-heading">
            {sessions.runLabel(group.run)}
            <!-- Classed rather than a bare `small`, because the heading is `text-transform:
                 uppercase` and an element selector under it would be a naked one. -->
            <span class="c-agents__run-note">
              {group.children.length}
              {group.children.length === 1 ? 'agent' : 'agents'}
              {#if group.parent}· started by {sessions.labelOf(group.parent)}{/if}
            </span>
          </h3>

          <ul class="o-plain-list c-agents__list">
            {#each group.children as child (child.id)}
              {@const status = sessions.statusOfPane(child)}
              <li class="c-agents__row" class:is-selected={visible.has(child.id)}>
                <SessionDot {status} />
                <span
                  class="c-agents__name"
                  title={child.agentTitle ?? sessions.labelOf(child)}
                  >{child.agentTitle ?? sessions.labelOf(child)}</span
                >
                <span class="c-agents__meta">
                  {sessions.labelOf(child)}{#if child.model}<span class="c-agents__model"
                      >{child.model}</span
                    >{/if}
                </span>
                <!-- Required beside the dot, not optional decoration: `_semantic.scss` forbids
                     state carried by colour alone, and `STATUS_WORD` is the other channel. -->
                <span class="c-agents__status">{STATUS_WORD[status] || 'idle'}</span>
                <span class="c-agents__actions">
                  <Button
                    variant="quiet"
                    size="sm"
                    disabled={visible.has(child.id)}
                    title="Replace the current tile with this session"
                    onclick={() => show(child.id, false)}
                  >
                    Show
                  </Button>
                  <Button
                    variant="quiet"
                    size="sm"
                    disabled={visible.has(child.id) || atTileCap}
                    title={atTileCap ? AT_TILE_CAP : 'Open this session beside the others'}
                    onclick={() => show(child.id, true)}
                  >
                    Split
                  </Button>
                  <Button
                    variant="danger-outline"
                    size="sm"
                    title="End this session and remove it"
                    onclick={() => closeChild(child.id)}
                  >
                    Close
                  </Button>
                </span>
              </li>
            {/each}
          </ul>
        </section>
      {/each}
    {/if}
  {/snippet}

  {#snippet footer()}
    <Button variant="neutral" onclick={onclose}>Done</Button>
  {/snippet}
</Dialog>

{#if closing}
  <Dialog
    title="Close session?"
    onclose={() => (closingId = null)}
    closeDisabled={closingBusy}
  >
    {#snippet body()}
      <p>
        Closing {closing.agentTitle ?? sessions.labelOf(closing)} ends that conversation and any
        children it started.
      </p>
    {/snippet}
    {#snippet footer()}
      <Button variant="neutral" onclick={() => (closingId = null)} disabled={closingBusy}
        >Cancel</Button
      >
      <Button
        variant="danger-solid"
        onclick={() => void closeConfirmed()}
        disabled={closingBusy}>{closingBusy ? 'Closing…' : 'Close'}</Button
      >
    {/snippet}
  </Dialog>
{/if}
