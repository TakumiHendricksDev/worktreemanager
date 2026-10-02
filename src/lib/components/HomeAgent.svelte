<script lang="ts">
  /**
   * Home's own conversation: an agent outside every repository, whose tools reach the sessions
   * inside them — or, until one is started, a launcher for it.
   *
   * The pane is an ordinary `SessionPane` with a `home` host, mounted here and nowhere else: it is
   * never placed in any worktree's layout, so there is no second copy to disagree with. One Home
   * conversation at a time; starting another closes this one, which stays under History exactly as
   * a restarted pane's conversation stays resumable.
   */
  import { HOME } from '../home';
  import { commands } from '../ipc/commands';
  import type { AgentOption, Resumable } from '../ipc/types';
  import { heading, item, popUp, separator, under, type MenuEntry } from '../native-menu';
  import { sessions } from '../state/sessions.svelte';
  import SessionPane from './SessionPane.svelte';
  import Button from './ui/Button.svelte';

  const { visible }: { visible: boolean } = $props();

  const pane = $derived(sessions.homePane);
  const past = $derived(sessions.resumable[HOME] ?? []);

  /** Every agent this machine has. No repository is here to decline one. */
  let agents = $state<AgentOption[]>([]);

  $effect(() => {
    if (!visible) return;
    void sessions.refreshResumable(HOME);
    void commands
      .listAgents(null)
      .then((list) => (agents = list))
      .catch(() => {});
    // A conversation restored from the last run comes back the first time Home is looked at, as a
    // worktree's panes do when theirs is.
    void sessions.materialiseHome();
  });

  function labelOf(provider: string): string {
    return agents.find((a) => a.id === provider)?.label ?? provider;
  }

  function titleOf(record: Resumable): string {
    const title = (record.title ?? 'Untitled conversation').replace(
      /^From Home \(wtm\):\s*/,
      '',
    );
    return title.length > 60 ? `${title.slice(0, 60)}…` : title;
  }

  function resume(record: Resumable) {
    void sessions.openHome(record.provider, {
      providerSession: record.providerSession,
      model: record.model,
      effort: record.effort,
    });
  }

  function history(event: MouseEvent) {
    const entries: MenuEntry[] = [
      heading('Start a new Home conversation'),
      ...agents
        .filter((a) => a.available)
        .map((a) => item(a.label, () => void sessions.openHome(a.id))),
    ];
    if (past.length > 0) {
      entries.push(separator, heading('Pick up a past one'));
      for (const record of past.slice(0, 12)) {
        entries.push(
          item(`${titleOf(record)} — ${labelOf(record.provider)}`, () => resume(record)),
        );
      }
    }
    void popUp(entries, under(event.currentTarget as Element));
  }
</script>

{#if pane}
  {#key `${pane.id}:${pane.generation}`}
    <SessionPane {pane} {visible} host={{ kind: 'home', onhistory: history }} />
  {/key}
{:else}
  <div class="c-home__intro">
    <h2 class="c-home__title">Home</h2>
    <p class="c-home__prose">
      Talk to an agent here that isn't in any repository. It can see every session in the
      tree, read what one has done, hand work to it or to a new session in any worktree, and
      keep talking to you while that work runs — it hears back as each session finishes.
      Approvals stay yours: whatever a session asks shows up under Needs you.
    </p>
    <div class="o-row">
      {#each agents as option (option.id)}
        <Button
          variant={option.available ? 'accent' : 'neutral'}
          size="sm"
          disabled={!option.available}
          title={option.detail ?? option.blurb}
          onclick={() => void sessions.openHome(option.id)}
        >
          {option.label}
        </Button>
      {/each}
    </div>
    {#if past.length > 0}
      <h3 class="c-section-heading c-home__past">Pick up a past Home conversation</h3>
      <ul class="o-plain-list c-home__resume">
        {#each past as record (record.provider + record.providerSession)}
          <li class="o-row">
            <Button
              variant="neutral"
              size="sm"
              title="Resume this conversation on {record.model ?? 'its own model'}"
              onclick={() => resume(record)}
            >
              {titleOf(record)}
            </Button>
            <span class="c-status--subtle">{labelOf(record.provider)}</span>
            <button
              class="c-row-action"
              title="Stop offering this conversation"
              onclick={() => void sessions.forget(HOME, record)}
            >
              forget
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}
