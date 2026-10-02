<script lang="ts">
  /**
   * What Home has handed out and not heard back about: which session has it, what it was asked,
   * and since when.
   *
   * # Why this exists beside the wires
   *
   * A wire is drawn while a message is in flight, and that used to be a few seconds of Home's own
   * turn. Now that Home hands work out and carries on, a delegation can be out for as long as the
   * work takes, while Home and the user talk about something else — and a dashed line between two
   * rows of a tree says that something is out, not what. This list is the what, and the reminder
   * that a notice is still to come for each row on it.
   *
   * Clicking a row peeks at the session doing the work, as an Activity row does.
   */
  import { fleet } from '../state/fleet.svelte';
  import { sessions, type Pane } from '../state/sessions.svelte';
  import { workspace } from '../state/workspace.svelte';
  import SessionDot from './ui/SessionDot.svelte';

  const { onpeek }: { onpeek: (pane: Pane) => void } = $props();

  const time = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });

  function where(pane: Pane): string {
    const project =
      workspace.projects.find((p) => p.id === pane.projectId)?.name ?? 'A project';
    const worktree =
      fleet.worktreesOf(pane.projectId)?.find((w) => w.id === pane.worktreeId)?.title ??
      pane.worktreeId.split('/').pop();
    return `${project} / ${worktree}`;
  }
</script>

{#if fleet.delegations.length > 0}
  <section class="c-inflight" aria-label="In flight">
    <h2 class="c-inflight__heading">
      In flight <span class="c-inflight__count">{fleet.delegations.length}</span>
      <span class="c-inflight__note">Home hears back as each one ends</span>
    </h2>
    <ul class="o-plain-list c-inflight__list">
      {#each fleet.delegations as exchange (exchange.id)}
        {@const pane = sessions.paneBySession(exchange.to)}
        {@const waiting = (pane?.approvals.length ?? 0) > 0}
        <li class="c-inflight__item">
          <button
            class="c-inflight__row"
            disabled={pane === null}
            onclick={() => pane && onpeek(pane)}
          >
            <span class="c-inflight__head">
              <SessionDot status={waiting ? 'attention' : 'working'} />
              <span class="c-inflight__who">{fleet.nameOf(exchange.to)}</span>
              <span class="c-inflight__state">{waiting ? 'waiting on you' : 'working'}</span
              >
              <time
                class="c-inflight__time"
                datetime={new Date(exchange.sentAt).toISOString()}
              >
                since {time.format(exchange.sentAt)}
              </time>
            </span>
            {#if pane}
              <span class="c-inflight__where">{where(pane)}</span>
            {/if}
            <span class="c-inflight__text">“{exchange.prompt}”</span>
          </button>
        </li>
      {/each}
    </ul>
  </section>
{/if}
